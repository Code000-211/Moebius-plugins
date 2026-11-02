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

/// A fake nono that says it is `version` and that Landlock is on.
#[cfg(unix)]
pub fn fake_nono(dir: &Path, version: &str) -> std::path::PathBuf {
    let path = dir.join("nono");
    script(
        &path,
        &format!(
            "#!/bin/sh\ncase \"$1\" in\n  --version) echo 'nono {version}' ;;\n  setup) echo '* Landlock enabled (syscall probe)'; echo '* TCP network rule support verified' ;;\nesac\n"
        ),
    );
    path
}
