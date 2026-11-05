//! The subcommands of this program that a wrapped launch uses. The core starts them itself, whether or not the plugin
//! is still running, so they are this program rather than a shell line:
//!
//! - `podman-sandbox run --podman <podman> --pass <names> -- <podman arguments>` is the wrapped program. It starts
//!   Podman with its own environment but for the variables named in `<names>`, writes those to Podman's standard
//!   input as `NAME=value` lines and an empty line (the container's [`ENV_SCRIPT`](crate::container::ENV_SCRIPT)
//!   reads them), then copies its standard input to Podman's until it ends. The standard output goes straight
//!   through. So the run token reaches the container without being in Podman's arguments, its environment or its
//!   container configuration.
//! - `podman-sandbox stop <podman> <container>` removes the container. It succeeds when there is nothing to remove,
//!   since the core runs it more than once for one process.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use moebius_plugin_api::CommandLine;

use crate::container::{NAME_PREFIX, env_lines};

/// The wrapped program's arguments, which start `podman <args>` with the variables `pass` given to the container.
pub fn run_args(podman: &str, pass: &[String], args: Vec<String>) -> Vec<String> {
    let mut all = vec![
        "run".to_string(),
        "--podman".into(),
        podman.into(),
        "--pass".into(),
        pass.join(","),
        "--".into(),
    ];
    all.extend(args);
    all
}

/// The command that removes the container `name` with the Podman at `podman`, in the environment `env` that Podman
/// needs to find its storage.
pub fn stop_command(
    exe: &Path,
    podman: &str,
    name: &str,
    env: &BTreeMap<String, String>,
) -> CommandLine {
    CommandLine {
        program: exe.to_string_lossy().into_owned(),
        args: vec!["stop".into(), podman.into(), name.into()],
        env: env.clone(),
    }
}

/// Whether `name` is named like a container of this plugin (`moebius-run-<id>-<nonce>`), so that the stop command
/// cannot be turned into a remover of other containers.
pub fn is_container_name(name: &str) -> bool {
    name.strip_prefix(NAME_PREFIX).is_some_and(|rest| {
        let mut parts = rest.splitn(2, '-');
        let id = parts.next().unwrap_or("");
        let nonce = parts.next().unwrap_or("");
        !id.is_empty()
            && id.bytes().all(|b| b.is_ascii_digit())
            && !nonce.is_empty()
            && nonce.bytes().all(|b| b.is_ascii_alphanumeric())
    })
}

/// What a subcommand did: the exit code, and a sentence to print when it did something unexpected.
#[derive(Debug, PartialEq, Eq)]
pub struct Outcome {
    pub code: i32,
    pub said: String,
}

/// Runs the subcommand in `args` (the arguments after the program's name); `None` when `args` is no subcommand.
pub fn run(args: &[String]) -> Option<Outcome> {
    match args {
        [cmd, flag, podman, pass_flag, pass, dashes, rest @ ..]
            if cmd == "run" && flag == "--podman" && pass_flag == "--pass" && dashes == "--" =>
        {
            let names: Vec<&str> = pass.split(',').filter(|n| !n.is_empty()).collect();
            Some(run_container(podman, &names, rest))
        }
        [cmd, podman, name] if cmd == "stop" => Some(Outcome {
            code: 0,
            said: stop(podman, name),
        }),
        _ => None,
    }
}

/// Starts Podman, gives the container its environment, relays the standard input, and exits as Podman did.
fn run_container(podman: &str, names: &[&str], args: &[String]) -> Outcome {
    let env: BTreeMap<String, String> = std::env::vars().collect();
    let lines = match env_lines(&env, names.iter().copied()) {
        Ok(lines) => lines,
        Err(e) => return failed(e.to_string()),
    };
    let podman_env = env
        .iter()
        .filter(|(name, _)| !names.contains(&name.as_str()));
    let child = Command::new(podman)
        .args(args)
        .env_clear()
        .envs(podman_env)
        .stdin(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(e) => return failed(format!("cannot run {podman}: {e}")),
    };
    let Some(mut input) = child.stdin.take() else {
        return failed("Podman's standard input is not a pipe".into());
    };
    // A container that ends before it reads its environment is told by its exit status, below.
    if input.write_all(lines.as_bytes()).is_ok() {
        std::thread::spawn(move || {
            let _ = std::io::copy(&mut std::io::stdin().lock(), &mut input);
        });
    }
    match child.wait() {
        Ok(status) => Outcome {
            code: exit_code(status),
            said: String::new(),
        },
        Err(e) => failed(format!("cannot wait for {podman}: {e}")),
    }
}

fn failed(said: String) -> Outcome {
    Outcome { code: 1, said }
}

#[cfg(unix)]
fn exit_code(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .or_else(|| status.signal().map(|signal| 128 + signal))
        .unwrap_or(1)
}

#[cfg(not(unix))]
fn exit_code(status: std::process::ExitStatus) -> i32 {
    status.code().unwrap_or(1)
}

/// Removes the container, waiting for nothing; says why when it could not.
fn stop(podman: &str, name: &str) -> String {
    if !is_container_name(name) {
        return format!("{name} is not a container of this plugin; left alone");
    }
    let status = Command::new(podman)
        .args(["rm", "--force", "--time", "0", "--ignore", name])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match status {
        Ok(status) if status.success() => String::new(),
        Ok(status) => format!("podman rm {name}: {status}"),
        Err(e) => format!("cannot run {podman}: {e}"),
    }
}

/// The path of this program, for the commands.
pub fn current_exe() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("podman-sandbox"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_containers_of_this_plugin_are_named_like_one() {
        assert!(is_container_name("moebius-run-7-18c3f"));
        assert!(!is_container_name("moebius-run-7"));
        assert!(!is_container_name("moebius-run-x-1"));
        assert!(!is_container_name("jig-shop"));
        assert!(!is_container_name("moebius-run-7-1;rm"));
    }

    #[test]
    fn the_wrapped_arguments_name_the_variables_not_their_values() {
        let args = run_args(
            "/usr/bin/podman",
            &["MOEBIUS_RUN_TOKEN".into(), "LANG".into()],
            vec!["run".into(), "--rm".into()],
        );
        assert_eq!(
            args,
            [
                "run",
                "--podman",
                "/usr/bin/podman",
                "--pass",
                "MOEBIUS_RUN_TOKEN,LANG",
                "--",
                "run",
                "--rm"
            ]
        );
    }

    #[test]
    fn arguments_that_are_no_subcommand_are_settings() {
        assert_eq!(run(&["--network".into(), "internet".into()]), None);
        assert_eq!(run(&["run".into(), "x".into()]), None);
        assert_eq!(run(&[]), None);
    }

    #[test]
    fn stop_leaves_other_containers_alone() {
        let said = run(&["stop".into(), "/does/not/exist".into(), "jig-shop".into()]).unwrap();
        assert_eq!(said.code, 0);
        assert!(said.said.contains("left alone"), "{said:?}");
    }

    #[cfg(unix)] // The fake Podman is a shell script.
    #[test]
    fn stop_asks_podman_and_a_failing_podman_does_not_fail_the_command() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log");
        let podman = dir.path().join("podman");
        let text = format!("#!/bin/sh\necho \"$@\" > {}\nexit 125\n", log.display());
        crate::testing::script(&podman, &text);
        std::fs::remove_file(&log).ok();
        let said = run(&[
            "stop".into(),
            podman.to_string_lossy().into(),
            "moebius-run-7-abc".into(),
        ])
        .unwrap();
        assert_eq!(said.code, 0);
        assert_eq!(
            std::fs::read_to_string(&log).unwrap().trim(),
            "rm --force --time 0 --ignore moebius-run-7-abc"
        );
        assert!(said.said.contains("125"), "{said:?}");
        let said = run(&[
            "stop".into(),
            "/does/not/exist".into(),
            "moebius-run-7-abc".into(),
        ])
        .unwrap();
        assert!(said.said.contains("cannot run"), "{said:?}");
    }
}
