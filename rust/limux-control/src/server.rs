use std::io;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::Path;
use std::sync::Arc;

use limux_protocol::{parse_v1_command_envelope, V2Request, V2Response};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Semaphore;
use tokio::time::{timeout, Duration};

use crate::{auth, Dispatcher};

const MAX_REQUEST_LEN: usize = 1024 * 1024;
const MAX_CONNECTIONS: usize = 64;
const CLIENT_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

pub async fn run_server<P: AsRef<Path>>(socket_path: P, dispatcher: Dispatcher) -> io::Result<()> {
    let socket_path = socket_path.as_ref();
    if socket_path.exists() {
        let metadata = std::fs::symlink_metadata(socket_path)?;
        if metadata.file_type().is_socket() {
            if std::os::unix::net::UnixStream::connect(socket_path).is_ok() {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    format!("socket already in use at {}", socket_path.display()),
                ));
            }
            std::fs::remove_file(socket_path)?;
        } else {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "refusing to overwrite non-socket path {}",
                    socket_path.display()
                ),
            ));
        }
    }
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    }

    let listener = {
        let old_umask = unsafe { libc::umask(0o177) };
        let result = UnixListener::bind(socket_path);
        unsafe { libc::umask(old_umask) };
        result?
    };
    serve(listener, dispatcher).await
}

pub async fn serve(listener: UnixListener, dispatcher: Dispatcher) -> io::Result<()> {
    let control_mode = auth::SocketControlMode::from_env();
    let server_pid = std::process::id();
    let semaphore = Arc::new(Semaphore::new(MAX_CONNECTIONS));

    loop {
        let (stream, _) = listener.accept().await?;
        let peer = match auth::authenticate_peer(&stream) {
            Ok(peer) => peer,
            Err(error) => {
                eprintln!("limux-control: failed to authenticate client: {error}");
                continue;
            }
        };
        if !auth::is_authorized(&peer, control_mode, server_pid) {
            eprintln!(
                "limux-control: rejected client pid={} uid={} mode={:?}",
                peer.pid, peer.uid, control_mode
            );
            continue;
        }

        let dispatcher = dispatcher.clone();
        let permit = match semaphore.clone().acquire_owned().await {
            Ok(permit) => permit,
            Err(_) => continue,
        };

        tokio::spawn(async move {
            let _permit = permit;
            if let Err(error) = handle_connection(stream, dispatcher).await {
                eprintln!("connection error: {error}");
            }
        });
    }
}

pub async fn handle_connection(stream: UnixStream, dispatcher: Dispatcher) -> io::Result<()> {
    let (reader_half, mut writer_half) = stream.into_split();
    let mut reader = BufReader::new(reader_half);
    let mut line_buf = Vec::with_capacity(4096);

    loop {
        line_buf.clear();
        let eof = loop {
            let available = match timeout(CLIENT_IDLE_TIMEOUT, reader.fill_buf()).await {
                Ok(result) => result?,
                Err(_) => return Ok(()),
            };

            if available.is_empty() {
                break true;
            }

            match available.iter().position(|byte| *byte == b'\n') {
                Some(position) => {
                    if line_buf.len() + position > MAX_REQUEST_LEN {
                        return Ok(());
                    }
                    line_buf.extend_from_slice(&available[..position]);
                    reader.consume(position + 1);
                    break false;
                }
                None => {
                    let len = available.len();
                    line_buf.extend_from_slice(available);
                    reader.consume(len);
                    if line_buf.len() > MAX_REQUEST_LEN {
                        return Ok(());
                    }
                }
            }
        };

        if eof && line_buf.is_empty() {
            return Ok(());
        }

        let incoming = std::str::from_utf8(&line_buf)
            .map(|line| line.trim_end_matches(['\n', '\r']))
            .unwrap_or("");
        if incoming.is_empty() {
            continue;
        }

        let response = match parse_request(incoming) {
            Ok(request) => dispatcher.dispatch(request).await,
            Err(message) => V2Response::error(None, -32700, message, None),
        };

        let mut payload = serde_json::to_string(&response)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
        payload.push('\n');

        writer_half.write_all(payload.as_bytes()).await?;
        writer_half.flush().await?;
    }
}

fn parse_request(incoming: &str) -> Result<V2Request, String> {
    if let Ok(request) = serde_json::from_str::<V2Request>(incoming) {
        return Ok(request);
    }

    match parse_v1_command_envelope(incoming) {
        Ok(v1) => Ok(v1.into_v2_request(None)),
        Err(error) => Err(format!("invalid request payload: {error}")),
    }
}
