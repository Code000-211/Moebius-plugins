//! The checks made once when the plugin starts: the platform can be sandboxed, nono is there and is the version the
//! plugin is tested with, and the settings ask nothing of the kernel that it cannot give. A plugin that fails them
//! exits with the sentence, so the core marks it not running and every launch is refused: a run never starts
//! unsandboxed because the sandbox is missing.

use std::process::Command;

use crate::settings::{Settings, UnixSockets};

/// Why the plugin cannot start.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CheckError {
    #[error("nono does not run on Windows outside WSL2, so this sandbox plugin cannot start here")]
    Windows,
    #[error("this platform ({0}) is not one nono sandboxes on: only Linux and macOS are")]
    Platform(String),
    #[error("cannot run {program:?}: {why}. Install nono {version} or set --nono to its path")]
    NotFound {
        program: String,
        why: String,
        version: String,
    },
    #[error("cannot tell the version of {program:?}: it printed {output:?}")]
    NoVersion { program: String, output: String },
    #[error(
        "nono {found} is installed but this plugin is tested with nono {wanted}; nono is before 1.0 and changes \
         its profiles between versions. Install {wanted}, or name the version you tested with --nono-version"
    )]
    Version { found: String, wanted: String },
    #[error(
        "the kernel does not offer Landlock, which nono needs on Linux (kernel 5.13 or later with Landlock on). \
         `nono setup --check-only` said: {0}"
    )]
    NoLandlock(String),
    #[error(
        "the kernel does not filter TCP connections by port (Landlock ABI 4, Linux 6.7 or later), which the network \
         mode \"moebius\" needs; every run in that mode would fail. Update the kernel, or set --network internet and \
         no --agent-network moebius. `nono setup --check-only` said: {0}"
    )]
    NoTcpFiltering(String),
    #[error(
        "this is WSL (kernel {0}), whose kernel refuses nono the seccomp listener that mediating Unix sockets needs, \
         so every run would fail. Set --unix-sockets allow to sandbox runs without it (a run can then connect to any \
         Unix socket its user can, such as Docker's, the D-Bus or the Windows interop socket, and act as the user \
         outside the sandbox through it), or run Moebius on native Linux. WSL2's kernel before 6.7 does not filter \
         TCP by port either, so there also set --network internet and no --agent-network moebius"
    )]
    Wsl2UnixSockets(String),
}

/// What `nono setup --check-only` says when the kernel filters TCP by port.
const TCP_FILTERING: [&str; 2] = ["TCP network filtering", "TCP network rule support verified"];

/// Checks the platform, nono's version and, on Linux, Landlock and WSL2. `os` is `std::env::consts::OS`, `kernel` the
/// kernel's release ([`kernel_release`]) and `wsl2` whether this is WSL2 ([`is_wsl2`]).
pub fn check(settings: &Settings, os: &str, kernel: &str, wsl2: bool) -> Result<(), CheckError> {
    match os {
        "windows" => return Err(CheckError::Windows),
        "linux" | "macos" => {}
        other => return Err(CheckError::Platform(other.to_string())),
    }
    // WSL2's init holds a seccomp listener already, so nono's own gets EBUSY and nono exits before the run starts.
    if os == "linux" && wsl2 && settings.unix_sockets == UnixSockets::Deny {
        return Err(CheckError::Wsl2UnixSockets(kernel.trim().to_string()));
    }
    let version = run(settings, &["--version"])?;
    let output = version.text();
    let found = output
        .split_whitespace()
        .last()
        .map(|word| word.trim_start_matches('v'))
        .filter(|word| word.starts_with(|c: char| c.is_ascii_digit()))
        .ok_or_else(|| CheckError::NoVersion {
            program: settings.nono.clone(),
            output: output.trim().to_string(),
        })?;
    if found != settings.nono_version.trim_start_matches('v') {
        return Err(CheckError::Version {
            found: found.to_string(),
            wanted: settings.nono_version.clone(),
        });
    }
    if os == "linux" {
        let setup = run(settings, &["setup", "--check-only"])?;
        let text = setup.text();
        if !setup.success || !text.contains("Landlock enabled") {
            let last = text.lines().rev().find(|l| !l.trim().is_empty());
            return Err(CheckError::NoLandlock(
                last.unwrap_or("nothing").trim().to_string(),
            ));
        }
        if settings.needs_tcp_filtering()
            && !TCP_FILTERING.iter().any(|phrase| text.contains(phrase))
        {
            let said = text
                .lines()
                .find(|l| l.contains("TCP"))
                .unwrap_or("nothing about TCP");
            return Err(CheckError::NoTcpFiltering(said.trim().to_string()));
        }
    }
    Ok(())
}

/// The release of the running kernel on Linux (it names Microsoft on WSL2), or nothing where it cannot be read.
pub fn kernel_release() -> String {
    std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default()
}

/// Whether this is WSL2, told the way nono tells it: by what the kernel shows, never by the environment, which a
/// process can set. The release names Microsoft on WSL2's own kernel; the interop entry is there with any kernel,
/// `WSLInterop-late` on WSL versions that start systemd first.
pub fn is_wsl2(kernel: &str) -> bool {
    kernel.to_ascii_lowercase().contains("microsoft")
        || ["WSLInterop", "WSLInterop-late"].iter().any(|name| {
            std::path::Path::new("/proc/sys/fs/binfmt_misc")
                .join(name)
                .exists()
        })
}

/// The absolute path of `program`: itself when it has a directory part, else the first `PATH` entry that holds it.
pub fn resolve(program: &str) -> Option<String> {
    let path = std::path::Path::new(program);
    let found = if path.components().count() > 1 {
        path.to_path_buf()
    } else {
        std::env::split_paths(&std::env::var_os("PATH")?)
            .map(|dir| dir.join(program))
            .find(|candidate| candidate.is_file())?
    };
    std::path::absolute(found)
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

struct Ran {
    success: bool,
    stdout: String,
    stderr: String,
}

impl Ran {
    fn text(&self) -> String {
        format!("{}\n{}", self.stdout, self.stderr)
    }
}

fn run(settings: &Settings, args: &[&str]) -> Result<Ran, CheckError> {
    let output = Command::new(&settings.nono)
        .args(args)
        .output()
        .map_err(|e| CheckError::NotFound {
            program: settings.nono.clone(),
            why: e.to_string(),
            version: settings.nono_version.clone(),
        })?;
    Ok(Ran {
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

// The fake nono is a shell script.
#[cfg(all(test, unix))]
mod tests {
    use std::path::Path;

    use super::*;

    /// A fake nono that prints `version` for `--version` and `setup` for `setup --check-only`, exiting with `code`.
    fn fake(dir: &Path, version: &str, setup: &str, code: i32) -> Settings {
        let path = dir.join("nono");
        let script = format!(
            "#!/bin/sh\ncase \"$1\" in\n  --version) echo '{version}' ;;\n  setup) echo '{setup}'; exit {code} ;;\nesac\n"
        );
        crate::testing::script(&path, &script);
        Settings {
            nono: path.to_string_lossy().into_owned(),
            ..Settings::default()
        }
    }

    #[test]
    fn the_pinned_version_with_landlock_passes() {
        let dir = tempfile::tempdir().unwrap();
        let settings = fake(
            dir.path(),
            "nono 0.81.0",
            "* Landlock enabled (syscall probe)\n* TCP network rule support verified",
            0,
        );
        assert_eq!(check(&settings, "linux", "", false), Ok(()));
        assert_eq!(check(&settings, "macos", "", false), Ok(()));
    }

    #[test]
    fn another_version_is_refused_and_the_setting_can_name_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut settings = fake(
            dir.path(),
            "nono 0.82.1",
            "Landlock enabled\nTCP network filtering",
            0,
        );
        let error = check(&settings, "linux", "", false).unwrap_err();
        assert_eq!(
            error,
            CheckError::Version {
                found: "0.82.1".into(),
                wanted: crate::PINNED_NONO_VERSION.into()
            }
        );
        assert!(error.to_string().contains("0.82.1"));
        settings.nono_version = "0.82.1".into();
        assert_eq!(check(&settings, "linux", "", false), Ok(()));
    }

    #[test]
    fn missing_landlock_refuses_on_linux_only() {
        let dir = tempfile::tempdir().unwrap();
        let settings = fake(
            dir.path(),
            "nono 0.81.0",
            "Landlock is not supported by this kernel",
            1,
        );
        let error = check(&settings, "linux", "", false).unwrap_err();
        assert!(matches!(error, CheckError::NoLandlock(_)));
        assert!(error.to_string().contains("not supported by this kernel"));
        // Seatbelt needs no such probe.
        assert_eq!(check(&settings, "macos", "", false), Ok(()));
    }

    #[test]
    fn mode_moebius_needs_tcp_filtering_at_start() {
        let dir = tempfile::tempdir().unwrap();
        let mut settings = fake(
            dir.path(),
            "nono 0.81.0",
            "* Landlock enabled (syscall probe)",
            0,
        );
        let error = check(&settings, "linux", "", false).unwrap_err();
        assert!(matches!(error, CheckError::NoTcpFiltering(_)), "{error}");
        assert!(error.to_string().contains("6.7"));
        // An agent in mode moebius needs it as much as the default.
        settings.network = crate::settings::Network::Internet;
        assert_eq!(check(&settings, "linux", "", false), Ok(()));
        settings
            .agent_network
            .insert("Coder".into(), crate::settings::Network::Moebius);
        assert!(matches!(
            check(&settings, "linux", "", false),
            Err(CheckError::NoTcpFiltering(_))
        ));
        let ok = fake(
            dir.path(),
            "nono 0.81.0",
            "* Landlock enabled\n* TCP network filtering available",
            0,
        );
        assert_eq!(check(&ok, "linux", "", false), Ok(()));
    }

    #[test]
    fn landlock_text_with_a_failing_exit_still_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let settings = fake(dir.path(), "nono 0.81.0", "Landlock enabled", 3);
        assert!(matches!(
            check(&settings, "linux", "", false),
            Err(CheckError::NoLandlock(_))
        ));
    }

    #[test]
    fn a_missing_nono_says_how_to_fix_it() {
        let settings = Settings {
            nono: "/does/not/exist/nono".into(),
            ..Settings::default()
        };
        let error = check(&settings, "linux", "", false)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("/does/not/exist/nono") && error.contains("--nono"),
            "{error}"
        );
    }

    #[test]
    fn output_without_a_version_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let settings = fake(dir.path(), "something else", "Landlock enabled", 0);
        assert!(matches!(
            check(&settings, "linux", "", false),
            Err(CheckError::NoVersion { .. })
        ));
    }
}

#[cfg(test)]
mod refusal_tests {
    use super::*;

    const WSL2: &str = "6.6.87.2-microsoft-standard-WSL2";

    #[test]
    fn wsl2_with_mediated_unix_sockets_is_refused_with_the_fix() {
        let settings = Settings {
            nono: "/does/not/exist".into(),
            ..Settings::default()
        };
        let error = check(&settings, "linux", WSL2, true).unwrap_err();
        assert_eq!(error, CheckError::Wsl2UnixSockets(WSL2.into()));
        for fix in ["--unix-sockets allow", "--network internet"] {
            assert!(error.to_string().contains(fix), "{error}");
        }
        // A WSL2 known by its interop entry under a kernel of the user's own is refused the same.
        assert!(matches!(
            check(&settings, "linux", "6.12.0", true),
            Err(CheckError::Wsl2UnixSockets(_))
        ));
        // Native Linux goes on to the checks of nono.
        assert!(matches!(
            check(&settings, "linux", "7.2.9-200.fc44.x86_64", false),
            Err(CheckError::NotFound { .. })
        ));
        let allowed = Settings {
            unix_sockets: UnixSockets::Allow,
            ..settings
        };
        assert!(matches!(
            check(&allowed, "linux", WSL2, true),
            Err(CheckError::NotFound { .. })
        ));
    }

    #[test]
    fn wsl2_is_told_by_the_kernel_release() {
        assert!(is_wsl2(WSL2));
        assert!(is_wsl2("5.15.167.4-microsoft-standard-WSL2"));
    }

    #[test]
    fn windows_and_unknown_platforms_are_refused_before_nono_is_run() {
        let settings = Settings {
            nono: "/does/not/exist".into(),
            ..Settings::default()
        };
        assert_eq!(
            check(&settings, "windows", "", false),
            Err(CheckError::Windows)
        );
        assert_eq!(
            check(&settings, "freebsd", "", false),
            Err(CheckError::Platform("freebsd".into()))
        );
    }
}
