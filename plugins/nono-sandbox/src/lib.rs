//! The nono sandbox launcher: a Moebius plugin of the kind `launcher` that starts every Pi-Bolt process inside a
//! [nono](https://github.com/nolabs-ai/nono) sandbox. The kernel enforces it (Landlock on Linux, Seatbelt on macOS).
//!
//! For each launch request the plugin writes an owner-only nono profile for the run and answers with
//! `nono run --profile <file> -- <program> <args>`, a stop command and a cleanup command that removes the profile.
//! At the server's start it removes the profiles of runs that no longer exist.
//!
//! The parts: [`settings`] (what `moebius.toml` passes as the plugin's arguments), [`check`] (nono's version and the
//! platform's sandbox support, checked once at start), [`profile`] (the profile of one run, as data), [`launch`] (the
//! plugin itself: the two methods, the files, the commands) and [`commands`] (the stop and cleanup commands, which
//! are this program started with a subcommand).

pub mod check;
pub mod commands;
pub mod launch;
pub mod profile;
pub mod settings;

/// The nono version this plugin is tested with. nono is before 1.0 and changes its profile format between minor
/// versions, so a different version is refused at start unless the setting `nono_version` names it.
pub const PINNED_NONO_VERSION: &str = "0.81.0";

/// The name the plugin is known by; the `[[plugins]]` entry must use it too.
pub const NAME: &str = "nono-sandbox";

/// Helpers for the tests that run a fake nono.
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
