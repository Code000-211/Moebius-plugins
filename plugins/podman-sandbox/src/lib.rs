//! The Podman sandbox launcher: a Moebius plugin of the kind `launcher` that starts every Pi-Bolt process inside a
//! rootless Podman container, from an image that holds the project's dependencies and tools.
//!
//! For each launch request the plugin answers with this program started as `podman-sandbox run`, which starts
//! `podman run` with the run's paths mounted at the same paths, Pi-Bolt's installation mounted read only, and a
//! network that reaches only the router's ports on the host's loopback (or, in mode `internet`, those and the
//! internet). The run's environment goes to the container over its standard input, never through Podman's arguments
//! or its container configuration. The stop command removes the container; at the server's start the containers of
//! runs that no longer exist are removed.
//!
//! The parts: [`settings`] (what `moebius.toml` passes as the plugin's arguments), [`check`] (the platform, Podman
//! and the images, checked once at start), [`container`] (the `podman run` arguments of one run, as data),
//! [`launch`] (the plugin itself: the two methods) and [`commands`] (the subcommands `run` and `stop` that the
//! wrapped launch and its stop command start).

pub mod check;
pub mod commands;
pub mod container;
pub mod launch;
pub mod settings;

/// The name the plugin is known by; the `[[plugins]]` entry must use it too.
pub const NAME: &str = "podman-sandbox";

/// Helpers for the tests that run a fake Podman.
#[cfg(all(test, unix))]
pub(crate) mod testing {
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    /// Writes an executable script. Another test's `fork` can hold the file open for writing for a moment, and
    /// running it then fails with "text file busy"; this waits until the file can be run.
    pub fn script(path: &Path, text: &str) {
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
}
