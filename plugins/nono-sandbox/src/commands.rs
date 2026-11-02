//! The stop and cleanup commands of a wrapped launch. The core runs them itself, with an environment cleared but for
//! `PATH` and `HOME`, whether or not the plugin is still running, so they are this program started with a subcommand
//! rather than a shell line: `nono-sandbox stop <nono> <session>` and `nono-sandbox cleanup <profile>`.
//!
//! Both succeed when there is nothing left to do, since the core runs them more than once for one process.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use moebius_plugin_api::CommandLine;

/// The prefix of a session's name, as `nono ps` shows it.
pub const SESSION_PREFIX: &str = "moebius-";

/// The command that ends the sandboxed session `session` of the nono at `nono`.
pub fn stop_command(exe: &Path, nono: &str, session: &str) -> CommandLine {
    CommandLine {
        program: exe.to_string_lossy().into_owned(),
        args: vec!["stop".into(), nono.into(), session.into()],
        env: Default::default(),
    }
}

/// The command that removes the profile file `profile`.
pub fn cleanup_command(exe: &Path, profile: &Path) -> CommandLine {
    CommandLine {
        program: exe.to_string_lossy().into_owned(),
        args: vec!["cleanup".into(), profile.to_string_lossy().into_owned()],
        env: Default::default(),
    }
}

/// Whether `path` is named like a profile this plugin writes (`run-<id>-<nonce>.json`), so that the cleanup command
/// cannot be turned into a remover of other files.
pub fn is_profile_name(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix("run-")?.strip_suffix(".json"))
        .is_some_and(|middle| {
            let mut parts = middle.splitn(2, '-');
            let id = parts.next().unwrap_or("");
            let nonce = parts.next().unwrap_or("");
            !id.is_empty()
                && id.bytes().all(|b| b.is_ascii_digit())
                && !nonce.is_empty()
                && nonce.bytes().all(|b| b.is_ascii_alphanumeric())
        })
}

/// Runs the subcommand in `args` (the arguments after the program's name); `None` when `args` is no subcommand.
/// The result is the sentence to print when it did something unexpected; the exit status stays 0 for that, so a run
/// is never held up by a stop that finds nothing to stop.
pub fn run(args: &[String]) -> Option<String> {
    match args {
        [cmd, nono, session] if cmd == "stop" => Some(stop(nono, session)),
        [cmd, profile] if cmd == "cleanup" => Some(cleanup(Path::new(profile))),
        _ => None,
    }
}

/// Asks nono to stop the session; says why when it could not (the session is usually gone already).
fn stop(nono: &str, session: &str) -> String {
    let status = Command::new(nono)
        .args(["--silent", "stop", "--force", session])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match status {
        Ok(status) if status.success() => String::new(),
        Ok(status) => format!("nono stop {session}: {status}; the session is probably gone"),
        Err(e) => format!("cannot run {nono}: {e}"),
    }
}

/// Removes the profile file; nothing to say when it is gone already.
fn cleanup(profile: &Path) -> String {
    if !is_profile_name(profile) {
        return format!(
            "{} is not a profile of this plugin; left alone",
            profile.display()
        );
    }
    match std::fs::remove_file(profile) {
        Ok(()) => String::new(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => format!("cannot remove {}: {e}", profile.display()),
    }
}

/// The path of this program, for the commands.
pub fn current_exe() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("nono-sandbox"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_profiles_of_this_plugin_are_named_like_one() {
        assert!(is_profile_name(Path::new("/tmp/p/run-7-18c3f.json")));
        assert!(!is_profile_name(Path::new("/tmp/p/run-7.json")));
        assert!(!is_profile_name(Path::new("/tmp/p/run-x-1.json")));
        assert!(!is_profile_name(Path::new("/etc/passwd")));
        assert!(!is_profile_name(Path::new("/tmp/p/run-7-1.json.bak")));
    }

    #[test]
    fn cleanup_removes_the_profile_and_succeeds_twice() {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().join("run-3-abc.json");
        std::fs::write(&profile, "{}").unwrap();
        assert_eq!(
            run(&["cleanup".into(), profile.to_string_lossy().into()]),
            Some(String::new())
        );
        assert!(!profile.exists());
        assert_eq!(
            run(&["cleanup".into(), profile.to_string_lossy().into()]),
            Some(String::new())
        );
    }

    #[test]
    fn cleanup_leaves_other_files_alone() {
        let dir = tempfile::tempdir().unwrap();
        let other = dir.path().join("secrets.toml");
        std::fs::write(&other, "x").unwrap();
        let said = run(&["cleanup".into(), other.to_string_lossy().into()]).unwrap();
        assert!(other.exists() && said.contains("left alone"), "{said}");
    }

    #[test]
    fn arguments_that_are_no_subcommand_are_settings() {
        assert_eq!(run(&["--network".into(), "internet".into()]), None);
        assert_eq!(run(&[]), None);
    }

    #[cfg(unix)] // The fake nono is a shell script.
    #[test]
    fn stop_asks_nono_and_a_failing_nono_does_not_fail_the_command() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log");
        let nono = dir.path().join("nono");
        let text = format!("#!/bin/sh\necho \"$@\" > {}\nexit 1\n", log.display());
        crate::testing::script(&nono, &text);
        std::fs::remove_file(&log).ok();
        let said = stop(&nono.to_string_lossy(), "moebius-run-7-abc");
        assert_eq!(
            std::fs::read_to_string(&log).unwrap().trim(),
            "--silent stop --force moebius-run-7-abc"
        );
        assert!(said.contains("probably gone"), "{said}");
        assert!(stop("/does/not/exist", "x").contains("cannot run"));
    }
}
