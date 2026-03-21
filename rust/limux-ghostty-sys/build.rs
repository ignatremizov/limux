use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let ghostty_root = std::env::var_os("LIMUX_GHOSTTY_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest_dir.join("../../ghostty"));
    let ghostty_lib = resolve_ghostty_lib_dir(&ghostty_root);

    println!("cargo:rustc-link-search=native={}", ghostty_lib.display());
    println!("cargo:rustc-link-lib=dylib=ghostty");
    println!("cargo:rustc-link-lib=dylib=epoxy");

    // Compile glad (GL loader) which libghostty depends on but doesn't
    // include when built as a shared library.
    let glad_src = ghostty_root.join("vendor/glad/src/gl.c");
    let glad_include = ghostty_root.join("vendor/glad/include");
    if glad_src.exists() {
        cc::Build::new()
            .file(&glad_src)
            .include(&glad_include)
            .compile("glad");
    }

    // Re-run if libghostty changes
    println!(
        "cargo:rerun-if-changed={}",
        ghostty_lib.join("libghostty.so").display()
    );
    println!("cargo:rerun-if-env-changed=LIMUX_GHOSTTY_ROOT");
    println!("cargo:rerun-if-env-changed=LIMUX_GHOSTTY_LIB_DIR");
}

fn resolve_ghostty_lib_dir(ghostty_root: &Path) -> PathBuf {
    if let Some(explicit) = std::env::var_os("LIMUX_GHOSTTY_LIB_DIR").map(PathBuf::from) {
        return explicit
            .canonicalize()
            .unwrap_or_else(|error| panic!("failed to resolve LIMUX_GHOSTTY_LIB_DIR: {error}"));
    }

    let ghostty_lib = ghostty_root.join("zig-out/lib");
    let library = ghostty_lib.join("libghostty.so");
    if !library.exists() {
        build_ghostty(ghostty_root);
    }

    ghostty_lib.canonicalize().unwrap_or_else(|error| {
        panic!("libghostty not found at {}: {error}", ghostty_lib.display())
    })
}

fn build_ghostty(ghostty_root: &Path) {
    assert!(
        ghostty_root.join("build.zig").exists(),
        "ghostty source not found at {}. Set LIMUX_GHOSTTY_ROOT or LIMUX_GHOSTTY_LIB_DIR.",
        ghostty_root.display()
    );

    let status = Command::new("zig")
        .arg("build")
        .arg("-Dapp-runtime=none")
        .arg("-Doptimize=ReleaseFast")
        .current_dir(ghostty_root)
        .status()
        .unwrap_or_else(|error| {
            panic!(
                "failed to build Ghostty at {} with zig: {error}",
                ghostty_root.display()
            )
        });

    assert!(
        status.success(),
        "zig build failed while building Ghostty at {} with status {status}",
        ghostty_root.display()
    );
}
