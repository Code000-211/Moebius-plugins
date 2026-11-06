//! Helpers the integration tests share.

#![allow(dead_code)] // Each test file uses some of them.

#[cfg(unix)]
use std::path::Path;

/// Writes an executable script. Another test's `fork` can hold a file open for writing for a moment, and running it
/// then fails with "text file busy"; this waits until it can be run.
#[cfg(unix)]
pub fn script(path: &Path, text: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    for _ in 0..500 {
        match std::process::Command::new(path).arg("--probe").output() {
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                std::thread::sleep(std::time::Duration::from_millis(10))
            }
            _ => return,
        }
    }
    panic!("{} stays busy", path.display());
}

/// A fake Podman: rootless with pasta, with the image `localhost/base`, and no containers. Its `run` writes its
/// arguments and environment to `run.args` and `run.env` beside it, then runs the container's command (the env
/// script and the program) on this machine, as a container would.
#[cfg(unix)]
pub fn fake_podman(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("podman");
    let at = dir.display();
    script(
        &path,
        &format!(
            r#"#!/bin/sh
case "$1" in
  info) echo '{{"host":{{"security":{{"rootless":true}},"pasta":{{"executable":"/usr/bin/pasta"}}}},"version":{{"Version":"5.8.7"}}}}' ;;
  image) [ "$3" = localhost/base ] ;;
  ps) echo '[]' ;;
  run)
    printf '%s\n' "$@" > '{at}/run.args'
    env > '{at}/run.env'
    while [ "$1" != localhost/base ]; do shift; done
    shift
    exec "$@" ;;
esac
"#
        ),
    );
    path
}
