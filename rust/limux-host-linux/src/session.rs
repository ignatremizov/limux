use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

#[derive(Debug, serde::Serialize, serde::Deserialize, Clone, Default, PartialEq, Eq)]
pub struct SavedWorkspace {
    pub name: String,
    pub favorite: bool,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub folder_path: Option<String>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize, Clone, Default, PartialEq, Eq)]
pub struct SavedSession {
    #[serde(default = "default_session_version")]
    pub version: u32,
    #[serde(default)]
    pub active_workspace_index: Option<usize>,
    #[serde(default = "default_sidebar_visible")]
    pub sidebar_visible: bool,
    #[serde(default)]
    pub sidebar_expanded_width: Option<i32>,
    #[serde(default)]
    pub workspaces: Vec<SavedWorkspace>,
}

const SESSION_FILE_NAME: &str = "session.json";
const LEGACY_WORKSPACES_FILE_NAME: &str = "workspaces.json";

fn default_session_version() -> u32 {
    1
}

fn default_sidebar_visible() -> bool {
    true
}

pub fn load_session_snapshot() -> SavedSession {
    let session_path = session_path();
    if session_path.exists() {
        return load_json_file::<SavedSession>(&session_path).unwrap_or_default();
    }

    let legacy_path = legacy_workspaces_path();
    if legacy_path.exists() {
        let workspaces = load_json_file::<Vec<SavedWorkspace>>(&legacy_path).unwrap_or_default();
        return SavedSession {
            version: default_session_version(),
            workspaces,
            ..SavedSession::default()
        };
    }

    SavedSession {
        version: default_session_version(),
        ..SavedSession::default()
    }
}

pub fn save_session_snapshot(session: &SavedSession) {
    let path = session_path();
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
        let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
    }

    let Ok(json) = serde_json::to_vec_pretty(session) else {
        return;
    };
    let _ = write_atomic(&path, &json);
}

fn load_json_file<T: for<'de> serde::Deserialize<'de>>(path: &Path) -> Option<T> {
    let payload = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str(&payload) {
        Ok(value) => Some(value),
        Err(_) => {
            let corrupt_path = path.with_extension(
                path.extension()
                    .map(|ext| format!("{}.corrupt", ext.to_string_lossy()))
                    .unwrap_or_else(|| "corrupt".to_string()),
            );
            let _ = std::fs::rename(path, corrupt_path);
            None
        }
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp_path = path.with_extension(format!("json.tmp.{}", std::process::id()));
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&tmp_path)?;
    file.write_all(bytes)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.sync_all()?;
    std::fs::rename(&tmp_path, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp_path);
    })?;
    Ok(())
}

fn data_dir() -> PathBuf {
    dirs::data_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join(".local/share")))
        .unwrap_or_else(|| std::env::temp_dir().join("limux"))
        .join("limux")
}

fn session_path() -> PathBuf {
    data_dir().join(SESSION_FILE_NAME)
}

fn legacy_workspaces_path() -> PathBuf {
    data_dir().join(LEGACY_WORKSPACES_FILE_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Mutex;

    static ENV_TEST_LOCK: Mutex<()> = Mutex::new(());

    struct EnvGuard {
        key: &'static str,
        old: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: Option<&str>) -> Self {
            let old = std::env::var_os(key);
            match value {
                Some(value) => unsafe { std::env::set_var(key, value) },
                None => unsafe { std::env::remove_var(key) },
            }
            Self { key, old }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.old {
                Some(value) => unsafe { std::env::set_var(self.key, value) },
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }

    fn with_data_home() -> (tempfile::TempDir, EnvGuard) {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let guard = EnvGuard::set(
            "XDG_DATA_HOME",
            Some(temp_dir.path().to_str().expect("xdg data dir utf8")),
        );
        (temp_dir, guard)
    }

    #[test]
    fn session_roundtrip_is_atomic_and_restorable() {
        let _lock = ENV_TEST_LOCK.lock().expect("env lock");
        let (_dir, _xdg) = with_data_home();

        let session = SavedSession {
            version: 1,
            active_workspace_index: Some(1),
            sidebar_visible: false,
            sidebar_expanded_width: Some(280),
            workspaces: vec![
                SavedWorkspace {
                    name: "alpha".to_string(),
                    favorite: true,
                    cwd: Some("/tmp/alpha".to_string()),
                    folder_path: None,
                },
                SavedWorkspace {
                    name: "beta".to_string(),
                    favorite: false,
                    cwd: None,
                    folder_path: Some("/tmp/beta".to_string()),
                },
            ],
        };

        save_session_snapshot(&session);

        assert_eq!(load_session_snapshot(), session);
        assert!(session_path().exists());
    }

    #[test]
    fn corrupt_session_file_is_renamed_and_ignored() {
        let _lock = ENV_TEST_LOCK.lock().expect("env lock");
        let (_dir, _xdg) = with_data_home();
        let path = session_path();
        std::fs::create_dir_all(path.parent().expect("session dir")).expect("create dir");
        std::fs::write(&path, "{not-json").expect("write corrupt payload");

        let loaded = load_session_snapshot();
        assert!(loaded.workspaces.is_empty());
        assert!(!path.exists());
        assert!(path.with_extension("json.corrupt").exists());
    }

    #[test]
    fn legacy_workspaces_file_is_migrated_on_read() {
        let _lock = ENV_TEST_LOCK.lock().expect("env lock");
        let (_dir, _xdg) = with_data_home();
        let path = legacy_workspaces_path();
        std::fs::create_dir_all(path.parent().expect("legacy dir")).expect("create dir");
        std::fs::write(
            &path,
            serde_json::to_vec(&vec![SavedWorkspace {
                name: "legacy".to_string(),
                favorite: false,
                cwd: Some("/tmp/legacy".to_string()),
                folder_path: None,
            }])
            .expect("serialize legacy"),
        )
        .expect("write legacy payload");

        let loaded = load_session_snapshot();
        assert_eq!(loaded.workspaces.len(), 1);
        assert_eq!(loaded.workspaces[0].name, "legacy");
    }
}
