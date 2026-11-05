//! The subcommands of this program that a wrapped launch uses. The core starts them itself, whether or not the plugin
//! is still running, so they are this program rather than a shell line:
//!
//! - `podman-sandbox run --podman <podman> --pass <names> [--build <image> <directory> [--base <image>]]… --
//!   <podman arguments>` is the wrapped program. With `--build`, when Podman has no `<image>` it first builds it from
//!   `<directory>/Dockerfile` with that directory as the context, its output on the standard error (the run's log),
//!   and fails when the build fails; `--base` gives that build the image it starts from as the build argument
//!   `MOEBIUS_BASE`. Builds go in the order given, so a base comes before what is built on it. Then it starts Podman with its own environment but for the variables named in `<names>`, writes those to
//!   Podman's standard
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

use crate::container::{BASE_ARG, NAME_PREFIX, PLUGIN_LABEL, env_lines};

/// An image to build from the Dockerfile in a directory when Podman does not have it, on the image `base` if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Build {
    pub image: String,
    pub dir: PathBuf,
    pub base: Option<String>,
}

/// The wrapped program's arguments, which start `podman <args>` with the variables `pass` given to the container,
/// after making the images `builds` name when Podman does not have them.
pub fn run_args(podman: &str, pass: &[String], builds: &[Build], args: Vec<String>) -> Vec<String> {
    let mut all = vec![
        "run".to_string(),
        "--podman".into(),
        podman.into(),
        "--pass".into(),
        pass.join(","),
    ];
    for build in builds {
        all.extend([
            "--build".into(),
            build.image.clone(),
            build.dir.to_string_lossy().into_owned(),
        ]);
        if let Some(base) = &build.base {
            all.extend(["--base".into(), base.clone()]);
        }
    }
    all.push("--".into());
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
        [cmd, flag, podman, pass_flag, pass, after @ ..]
            if cmd == "run" && flag == "--podman" && pass_flag == "--pass" =>
        {
            let (builds, rest) = parse_builds(after)?;
            let names: Vec<&str> = pass.split(',').filter(|n| !n.is_empty()).collect();
            Some(run_container(podman, &names, &builds, rest))
        }
        [cmd, podman, name] if cmd == "stop" => Some(Outcome {
            code: 0,
            said: stop(podman, name),
        }),
        _ => None,
    }
}

/// The builds that start `args` and the Podman arguments after the `--` that ends them; `None` when `args` is not
/// that.
fn parse_builds(args: &[String]) -> Option<(Vec<Build>, &[String])> {
    let mut builds = Vec::new();
    let mut rest = args;
    loop {
        match rest {
            [dashes, after @ ..] if dashes == "--" => return Some((builds, after)),
            [flag, image, dir, after @ ..] if flag == "--build" => {
                let (base, after) = match after {
                    [flag, base, after @ ..] if flag == "--base" => (Some(base.clone()), after),
                    _ => (None, after),
                };
                builds.push(Build {
                    image: image.clone(),
                    dir: PathBuf::from(dir),
                    base,
                });
                rest = after;
            }
            _ => return None,
        }
    }
}

/// Builds the images asked for that Podman lacks, starts Podman, gives the container its environment, relays the
/// standard input, and exits as Podman did.
fn run_container(podman: &str, names: &[&str], builds: &[Build], args: &[String]) -> Outcome {
    let env: BTreeMap<String, String> = std::env::vars().collect();
    let lines = match env_lines(&env, names.iter().copied()) {
        Ok(lines) => lines,
        Err(e) => return failed(e.to_string()),
    };
    let podman_env: BTreeMap<&String, &String> = env
        .iter()
        .filter(|(name, _)| !names.contains(&name.as_str()))
        .collect();
    for build in builds {
        if let Err(why) = ensure_image(podman, &podman_env, build) {
            return failed(why);
        }
    }
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

/// Builds the image from the directory of its Dockerfile, unless Podman has it already. The build's output goes to the
/// standard error, since the standard output is the harness's protocol.
fn ensure_image(
    podman: &str,
    env: &BTreeMap<&String, &String>,
    build: &Build,
) -> Result<(), String> {
    let Build { image, dir, base } = build;
    let podman_command = || {
        let mut command = Command::new(podman);
        command.env_clear().envs(env.iter().map(|(k, v)| (*k, *v)));
        command
    };
    let exists = podman_command()
        .args(["image", "exists", image])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("cannot run {podman}: {e}"))?;
    if exists.success() {
        return Ok(());
    }
    eprintln!("podman-sandbox: building {image} from a Dockerfile");
    let mut command = podman_command();
    command
        .arg("build")
        .args(["--tag", image, "--label"])
        .arg(format!("{PLUGIN_LABEL}={}", crate::NAME));
    if let Some(base) = base {
        command.arg("--build-arg").arg(format!("{BASE_ARG}={base}"));
    }
    let status = command
        .arg("--file")
        .arg(dir.join("Dockerfile"))
        .arg(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::from(std::io::stderr()))
        .status()
        .map_err(|e| format!("cannot run {podman}: {e}"))?;
    match status.success() {
        true => Ok(()),
        false => Err(format!(
            "cannot build the image {image} from its Dockerfile: podman build failed ({status}); its output is above"
        )),
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
            &[],
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
    fn a_build_is_named_before_the_podman_arguments() {
        let args = run_args(
            "podman",
            &[],
            &[Build {
                image: "localhost/moebius-podman-sandbox:ab".into(),
                dir: "/tmp/n-build".into(),
                base: None,
            }],
            vec!["run".into()],
        );
        assert_eq!(
            args,
            [
                "run",
                "--podman",
                "podman",
                "--pass",
                "",
                "--build",
                "localhost/moebius-podman-sandbox:ab",
                "/tmp/n-build",
                "--",
                "run"
            ]
        );
    }

    #[cfg(unix)] // The fake Podman is a shell script.
    #[test]
    fn run_builds_a_missing_image_once_and_fails_without_starting_the_container_when_the_build_fails()
     {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log");
        let podman = dir.path().join("podman");
        // `image exists` answers from a marker file, which a build that succeeds makes; `build` fails when the
        // Dockerfile says so.
        let text = format!(
            "#!/bin/sh\necho \"$@\" >> {log}\ncase \"$1\" in\n  image) [ -e {made} ] ;;\n  build) grep -q fail \"$7\" && exit 3; touch {made} ;;\n  *) exit 0 ;;\nesac\n",
            log = log.display(),
            made = dir.path().join("made").display(),
        );
        crate::testing::script(&podman, &text);
        let context = dir.path().join("n-build");
        std::fs::create_dir(&context).unwrap();
        let start = |dockerfile: &str| {
            std::fs::write(context.join("Dockerfile"), dockerfile).unwrap();
            std::fs::write(&log, "").unwrap();
            let args = run_args(
                &podman.to_string_lossy(),
                &[],
                &[Build {
                    image: "localhost/moebius-podman-sandbox:ab".into(),
                    dir: context.clone(),
                    base: None,
                }],
                vec!["run".into(), "--rm".into()],
            );
            let outcome = run(&args).unwrap();
            let calls: Vec<String> = std::fs::read_to_string(&log)
                .unwrap()
                .lines()
                .map(String::from)
                .collect();
            (outcome, calls)
        };

        let (outcome, calls) = start("FROM fail\n");
        assert_eq!(outcome.code, 1);
        assert!(outcome.said.contains("cannot build"), "{outcome:?}");
        assert_eq!(
            calls.len(),
            2,
            "no container after a failed build: {calls:?}"
        );

        let (outcome, calls) = start("FROM debian\n");
        assert_eq!(outcome.code, 0, "{outcome:?}");
        let file = context.join("Dockerfile");
        assert_eq!(
            calls,
            [
                "image exists localhost/moebius-podman-sandbox:ab".to_string(),
                format!(
                    "build --tag localhost/moebius-podman-sandbox:ab --label moebius.plugin=podman-sandbox --file {} {}",
                    file.display(),
                    context.display()
                ),
                "run --rm".to_string(),
            ]
        );

        // Built once, it is not built again.
        let (_, calls) = start("FROM debian\n");
        assert_eq!(
            calls,
            [
                "image exists localhost/moebius-podman-sandbox:ab",
                "run --rm"
            ]
        );
    }

    #[test]
    fn builds_are_named_in_order_and_a_build_on_a_base_names_it() {
        let builds = [
            Build {
                image: "base:1".into(),
                dir: "/tmp/n-0-build".into(),
                base: None,
            },
            Build {
                image: "repo:2".into(),
                dir: "/tmp/n-1-build".into(),
                base: Some("base:1".into()),
            },
        ];
        let args = run_args("podman", &[], &builds, vec!["run".into()]);
        assert_eq!(
            args[5..],
            [
                "--build",
                "base:1",
                "/tmp/n-0-build",
                "--build",
                "repo:2",
                "/tmp/n-1-build",
                "--base",
                "base:1",
                "--",
                "run"
            ]
        );
        let (parsed, rest) = parse_builds(&args[5..]).unwrap();
        assert_eq!(parsed, builds);
        assert_eq!(rest, ["run"]);
        // Anything else than builds and the `--` that ends them is no subcommand.
        assert_eq!(parse_builds(&["--build".into(), "x".into()]), None);
        assert_eq!(parse_builds(&["--base".into(), "x".into()]), None);
    }

    #[cfg(unix)] // The fake Podman is a shell script.
    #[test]
    fn run_builds_the_base_first_and_gives_it_to_the_build_on_it() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log");
        let podman = dir.path().join("podman");
        // Nothing exists until it is built.
        let text = format!(
            "#!/bin/sh\necho \"$@\" >> {log}\ncase \"$1\" in\n  image) [ -e {made}/\"$3\" ] ;;\n  build) touch {made}/\"$3\" ;;\n  *) exit 0 ;;\nesac\n",
            log = log.display(),
            made = dir.path().display(),
        );
        crate::testing::script(&podman, &text);
        let make = |name: &str| {
            let context = dir.path().join(name);
            std::fs::create_dir(&context).unwrap();
            std::fs::write(context.join("Dockerfile"), "FROM debian\n").unwrap();
            context
        };
        let builds = [
            Build {
                image: "base".into(),
                dir: make("base-build"),
                base: None,
            },
            Build {
                image: "repo".into(),
                dir: make("repo-build"),
                base: Some("base".into()),
            },
        ];
        let args = run_args(&podman.to_string_lossy(), &[], &builds, vec!["run".into()]);
        std::fs::write(&log, "").unwrap();
        let outcome = run(&args).unwrap();
        assert_eq!(outcome.code, 0, "{outcome:?}");
        let calls: Vec<String> = std::fs::read_to_string(&log)
            .unwrap()
            .lines()
            .map(String::from)
            .collect();
        let build_call = |image: &str, extra: &str, name: &str| {
            format!(
                "build --tag {image} --label moebius.plugin=podman-sandbox {extra}--file {} {}",
                dir.path().join(name).join("Dockerfile").display(),
                dir.path().join(name).display()
            )
        };
        assert_eq!(
            calls,
            [
                "image exists base".to_string(),
                build_call("base", "", "base-build"),
                "image exists repo".to_string(),
                build_call("repo", "--build-arg MOEBIUS_BASE=base ", "repo-build"),
                "run".to_string(),
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
