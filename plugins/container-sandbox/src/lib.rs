//! The container sandbox launcher: a Moebius plugin of the kind `launcher` that starts every Pi-Bolt process inside a
//! container of rootless Podman or of Docker, from an image that holds the project's dependencies and tools. Both
//! engines build the same Dockerfiles and give a run the same view of the host.
//!
//! For each launch request the plugin answers with this program started as `container-sandbox run`, which starts the
//! engine's `run` with the run's paths mounted at the same paths, Pi-Bolt's installation mounted read only, and a
//! network that reaches only the router's ports on the host's loopback (or, in mode `internet`, those and the
//! internet). The run's environment goes to the container over its standard input, never through the engine's
//! arguments or its container configuration. The stop command removes the container; at the server's start the
//! containers of runs that no longer exist are removed.
//!
//! The parts: [`settings`] (what `moebius.toml` passes as the plugin's arguments), [`check`] (the platform, the
//! engine and the images, checked once at start), [`container`] (the `run` arguments of one run, as data),
//! [`mounts`] (the folders and volumes a person gives a repository's runs), [`launch`] (the plugin itself: the two
//! methods), [`commands`] (the subcommands `run`, `stop` and `relay` that the wrapped launch, its stop command and a
//! Docker container start), [`relay`] (how a Docker run reaches the router's ports, which Podman's pasta does) and
//! [`page`] (its page for its settings in the web app, with presets).

pub mod check;
pub mod commands;
pub mod container;
pub mod launch;
pub mod mounts;
pub mod page;
#[cfg(unix)]
pub mod relay;
pub mod settings;

/// The name the plugin is known by; the `[[plugins]]` entry must use it too.
pub const NAME: &str = "container-sandbox";

/// Helpers for the tests that run a fake engine.
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
