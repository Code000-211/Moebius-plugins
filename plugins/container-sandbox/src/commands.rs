//! The subcommands of this program that a wrapped launch uses. The core starts them itself, whether or not the plugin
//! is still running, so they are this program rather than a shell line:
//!
//! - `container-sandbox run --engine <podman|docker> <program> --pass <names> [--build <image> <directory> [--base
//!   <image>]]… [--own <image> <uid>:<gid> <volumes>] [--relay <directory> <ports>] -- <engine arguments>` is the
//!   wrapped program. With `--build`, when the engine has no `<image>` it first builds it from
//!   `<directory>/Dockerfile` with that directory as the context, its output on the standard error (the run's log),
//!   and fails when the build fails; `--base` gives that build the image it starts from as the build argument
//!   `MOEBIUS_BASE`. Builds go in the order given, so a base comes before what is built on it. With `--own` (Docker,
//!   which cannot make a volume the run's user's as it mounts it) a container of `<image>` that can do nothing but
//!   change owners gives the named volumes to the user. With `--relay` (Docker) it listens on the Unix sockets of the
//!   ports in `<directory>` for as long as it runs ([`crate::relay::outside`]). Then it starts the engine with its own
//!   environment but for the variables named in `<names>`, writes those to the engine's standard input as
//!   `NAME=value` lines and an empty line (the container's [`ENV_SCRIPT`](crate::container::ENV_SCRIPT) reads them),
//!   then copies its standard input to the engine's until it ends. The standard output goes straight through. So the
//!   run token reaches the container without being in the engine's arguments, its environment or its container
//!   configuration.
//! - `container-sandbox stop <podman|docker> <program> <container>` removes the container. It succeeds when there is
//!   nothing to remove, since the core runs it more than once for one process.
//! - `container-sandbox relay <directory> <ports> -- <command>…` is the container's side of the relay, in a Docker
//!   container: it listens on the ports of the container's loopback ([`crate::relay::inside`]), then runs the command
//!   and exits as it did.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use moebius_plugin_api::CommandLine;

use crate::check::image_exists;
use crate::container::{BASE_ARG, NAME_PREFIX, PLUGIN_LABEL, User, env_lines};
use crate::settings::Engine;

/// An image to build from the Dockerfile in a directory when the engine does not have it, on the image `base` if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Build {
    pub image: String,
    pub dir: PathBuf,
    pub base: Option<String>,
}

/// Named volumes to give to `user` with a container of `image` before the run's starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Own {
    pub image: String,
    pub user: User,
    pub volumes: Vec<String>,
}

/// The directory of a Docker run's relay sockets, and the ports it relays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relay {
    pub dir: PathBuf,
    pub ports: Vec<u16>,
}

/// What the wrapped program does: everything `container-sandbox run` reads from its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wrap {
    pub engine: Engine,
    pub program: String,
    /// The variables given to the container.
    pub pass: Vec<String>,
    pub builds: Vec<Build>,
    pub own: Option<Own>,
    pub relay: Option<Relay>,
    /// The engine's arguments that start the container.
    pub args: Vec<String>,
}

impl Wrap {
    /// The wrapped program's arguments.
    pub fn to_args(&self) -> Vec<String> {
        let mut all = vec![
            "run".to_string(),
            "--engine".into(),
            self.engine.name().into(),
            self.program.clone(),
            "--pass".into(),
            self.pass.join(","),
        ];
        for build in &self.builds {
            all.extend([
                "--build".into(),
                build.image.clone(),
                build.dir.to_string_lossy().into_owned(),
            ]);
            if let Some(base) = &build.base {
                all.extend(["--base".into(), base.clone()]);
            }
        }
        if let Some(own) = &self.own {
            all.extend([
                "--own".into(),
                own.image.clone(),
                format!("{}:{}", own.user.uid, own.user.gid),
                own.volumes.join(","),
            ]);
        }
        if let Some(relay) = &self.relay {
            let ports: Vec<String> = relay.ports.iter().map(u16::to_string).collect();
            all.extend([
                "--relay".into(),
                relay.dir.to_string_lossy().into_owned(),
                ports.join(","),
            ]);
        }
        all.push("--".into());
        all.extend(self.args.iter().cloned());
        all
    }

    /// Reads the arguments after `run`; `None` when they are not what [`Wrap::to_args`] writes.
    fn parse(args: &[String]) -> Option<Self> {
        let [flag, engine, program, pass_flag, pass, rest @ ..] = args else {
            return None;
        };
        let mut rest = rest;
        if flag != "--engine" || pass_flag != "--pass" {
            return None;
        }
        let mut wrap = Self {
            engine: engine_named(engine)?,
            program: program.clone(),
            pass: pass
                .split(',')
                .filter(|n| !n.is_empty())
                .map(String::from)
                .collect(),
            builds: Vec::new(),
            own: None,
            relay: None,
            args: Vec::new(),
        };
        loop {
            match rest {
                [dashes, after @ ..] if dashes == "--" => {
                    wrap.args = after.to_vec();
                    return Some(wrap);
                }
                [flag, image, dir, after @ ..] if flag == "--build" => {
                    let (base, after) = match after {
                        [flag, base, after @ ..] if flag == "--base" => (Some(base.clone()), after),
                        _ => (None, after),
                    };
                    wrap.builds.push(Build {
                        image: image.clone(),
                        dir: PathBuf::from(dir),
                        base,
                    });
                    rest = after;
                }
                [flag, image, ids, volumes, after @ ..] if flag == "--own" => {
                    let (uid, gid) = ids.split_once(':')?;
                    wrap.own = Some(Own {
                        image: image.clone(),
                        user: User {
                            uid: uid.parse().ok()?,
                            gid: gid.parse().ok()?,
                        },
                        volumes: volumes
                            .split(',')
                            .filter(|v| !v.is_empty())
                            .map(String::from)
                            .collect(),
                    });
                    rest = after;
                }
                [flag, dir, ports, after @ ..] if flag == "--relay" => {
                    wrap.relay = Some(Relay {
                        dir: PathBuf::from(dir),
                        ports: port_list(ports)?,
                    });
                    rest = after;
                }
                _ => return None,
            }
        }
    }
}

/// The ports of a list such as `7172,7173`; `None` when it is not one.
fn port_list(list: &str) -> Option<Vec<u16>> {
    list.split(',')
        .filter(|port| !port.is_empty())
        .map(|port| port.parse().ok())
        .collect()
}

fn engine_named(name: &str) -> Option<Engine> {
    [Engine::Podman, Engine::Docker]
        .into_iter()
        .find(|engine| engine.name() == name)
}

/// The command that removes the container `name` with the engine's program `program`, in the environment `env` that
/// the engine needs to find its storage.
pub fn stop_command(
    exe: &Path,
    engine: Engine,
    program: &str,
    name: &str,
    env: &BTreeMap<String, String>,
) -> CommandLine {
    CommandLine {
        program: exe.to_string_lossy().into_owned(),
        args: vec![
            "stop".into(),
            engine.name().into(),
            program.into(),
            name.into(),
        ],
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
        [cmd, rest @ ..] if cmd == "run" => {
            let wrap = Wrap::parse(rest)?;
            Some(run_container(&wrap))
        }
        [cmd, engine, program, name] if cmd == "stop" => Some(Outcome {
            code: 0,
            said: stop(engine_named(engine)?, program, name),
        }),
        [cmd, dir, ports, dashes, command @ ..] if cmd == "relay" && dashes == "--" => {
            let ports = port_list(ports)?;
            Some(relay_and_run(Path::new(dir), &ports, command))
        }
        _ => None,
    }
}

/// Builds the images asked for that the engine lacks, gives the volumes to the user, starts the relay, starts the
/// engine, gives the container its environment, relays the standard input, and exits as the engine did.
fn run_container(wrap: &Wrap) -> Outcome {
    let program = &wrap.program;
    let env: BTreeMap<String, String> = std::env::vars().collect();
    let lines = match env_lines(&env, wrap.pass.iter().map(String::as_str)) {
        Ok(lines) => lines,
        Err(e) => return failed(e.to_string()),
    };
    let engine_env: BTreeMap<&String, &String> = env
        .iter()
        .filter(|(name, _)| !wrap.pass.contains(name))
        .collect();
    let engine_command = || {
        let mut command = Command::new(program);
        command
            .env_clear()
            .envs(engine_env.iter().map(|(k, v)| (*k, *v)));
        command
    };
    for build in &wrap.builds {
        if let Err(why) = ensure_image(wrap.engine, program, &engine_command, build) {
            return failed(why);
        }
    }
    if let Some(own) = &wrap.own
        && let Err(why) = own_volumes(program, &engine_command, own)
    {
        return failed(why);
    }
    if let Some(relay) = &wrap.relay
        && let Err(why) = start_relay(relay)
    {
        return failed(why);
    }
    let child = engine_command()
        .args(&wrap.args)
        .stdin(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(e) => return failed(format!("cannot run {program}: {e}")),
    };
    let Some(mut input) = child.stdin.take() else {
        return failed(format!(
            "the standard input of {} is not a pipe",
            wrap.engine.title()
        ));
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
        Err(e) => failed(format!("cannot wait for {program}: {e}")),
    }
}

/// Builds the image from the directory of its Dockerfile, unless the engine has it already. The build's output goes to
/// the standard error, since the standard output is the harness's protocol.
fn ensure_image(
    engine: Engine,
    program: &str,
    engine_command: &dyn Fn() -> Command,
    build: &Build,
) -> Result<(), String> {
    let Build { image, dir, base } = build;
    let exists = engine_command()
        .args(image_exists(engine, image))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("cannot run {program}: {e}"))?;
    if exists.success() {
        return Ok(());
    }
    eprintln!(
        "{}: building {image} from a Dockerfile with {}",
        crate::NAME,
        engine.title()
    );
    let mut command = engine_command();
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
        .map_err(|e| format!("cannot run {program}: {e}"))?;
    match status.success() {
        true => Ok(()),
        false => Err(format!(
            "cannot build the image {image} from its Dockerfile: {} build failed ({status}); its output is above",
            engine.name()
        )),
    }
}

/// The engine's arguments of the container that gives the volumes to the user: root in it, but with no capability but
/// changing owners, no network, and nothing mounted but the volumes.
pub fn own_args(own: &Own) -> Vec<String> {
    let mut args: Vec<String> = [
        "run",
        "--rm",
        "--pull",
        "never",
        "--log-driver",
        "none",
        "--network",
        "none",
        "--user",
        "0:0",
        "--cap-drop",
        "ALL",
        "--cap-add",
        "CHOWN",
        "--security-opt",
        "no-new-privileges",
        "--label",
    ]
    .map(String::from)
    .into();
    args.push(format!("{PLUGIN_LABEL}={}", crate::NAME));
    let at = |index: usize| format!("/moebius-volumes/{index}");
    for (index, volume) in own.volumes.iter().enumerate() {
        args.push("--volume".into());
        args.push(format!("{volume}:{}:z", at(index)));
    }
    args.extend([
        "--entrypoint".into(),
        "chown".into(),
        own.image.clone(),
        format!("{}:{}", own.user.uid, own.user.gid),
    ]);
    args.extend((0..own.volumes.len()).map(at));
    args
}

fn own_volumes(
    program: &str,
    engine_command: &dyn Fn() -> Command,
    own: &Own,
) -> Result<(), String> {
    let status = engine_command()
        .args(own_args(own))
        .stdin(Stdio::null())
        .stdout(Stdio::from(std::io::stderr()))
        .status()
        .map_err(|e| format!("cannot run {program}: {e}"))?;
    match status.success() {
        true => Ok(()),
        false => Err(format!(
            "cannot give the volumes {} to the run's user with chown in the image {} ({status}); its output is above",
            own.volumes.join(", "),
            own.image
        )),
    }
}

#[cfg(unix)]
fn start_relay(relay: &Relay) -> Result<(), String> {
    crate::relay::outside(&relay.dir, &relay.ports).map_err(|e| e.to_string())
}

#[cfg(unix)]
fn start_relay_inside(dir: &Path, ports: &[u16]) -> Result<(), String> {
    crate::relay::inside(dir, ports).map_err(|e| e.to_string())
}

#[cfg(not(unix))]
fn start_relay(_: &Relay) -> Result<(), String> {
    Err(NO_RELAY.into())
}

#[cfg(not(unix))]
fn start_relay_inside(_: &Path, _: &[u16]) -> Result<(), String> {
    Err(NO_RELAY.into())
}

#[cfg(not(unix))]
const NO_RELAY: &str = "the relay of the router's ports runs on Linux only";

/// The container's side of the relay: listens, then runs `command` and exits as it did.
fn relay_and_run(dir: &Path, ports: &[u16], command: &[String]) -> Outcome {
    if let Err(why) = start_relay_inside(dir, ports) {
        return failed(why);
    }
    let [program, args @ ..] = command else {
        return failed("the relay has no command to run".into());
    };
    match Command::new(program).args(args).status() {
        Ok(status) => Outcome {
            code: exit_code(status),
            said: String::new(),
        },
        Err(e) => failed(format!("cannot run {program}: {e}")),
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

/// The arguments after the engine's program that remove the container `name` at once.
pub fn remove_args(engine: Engine, name: &str) -> Vec<String> {
    match engine {
        Engine::Podman => ["rm", "--force", "--time", "0", "--ignore", name]
            .map(String::from)
            .into(),
        // Docker's `rm --force` kills at once and has no `--ignore`; a container already gone is told by its words.
        Engine::Docker => ["rm", "--force", name].map(String::from).into(),
    }
}

/// Removes the container, waiting for nothing; says why when it could not.
fn stop(engine: Engine, program: &str, name: &str) -> String {
    if !is_container_name(name) {
        return format!("{name} is not a container of this plugin; left alone");
    }
    let output = Command::new(program)
        .args(remove_args(engine, name))
        .stdin(Stdio::null())
        .output();
    match output {
        Ok(output) if output.status.success() => String::new(),
        Ok(output)
            if engine == Engine::Docker
                && String::from_utf8_lossy(&output.stderr).contains("No such container") =>
        {
            String::new()
        }
        Ok(output) => format!("{} rm {name}: {}", engine.name(), output.status),
        Err(e) => format!("cannot run {program}: {e}"),
    }
}

/// The path of this program, for the commands.
pub fn current_exe() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from(crate::NAME))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wrap(program: &str, builds: Vec<Build>, args: &[&str]) -> Wrap {
        Wrap {
            engine: Engine::Podman,
            program: program.into(),
            pass: Vec::new(),
            builds,
            own: None,
            relay: None,
            args: args.iter().map(|a| a.to_string()).collect(),
        }
    }

    #[cfg(unix)]
    fn calls(log: &Path) -> Vec<String> {
        std::fs::read_to_string(log)
            .unwrap()
            .lines()
            .map(String::from)
            .collect()
    }

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
        let mut wrap = wrap("/usr/bin/podman", vec![], &["run", "--rm"]);
        wrap.pass = vec!["MOEBIUS_RUN_TOKEN".into(), "LANG".into()];
        assert_eq!(
            wrap.to_args(),
            [
                "run",
                "--engine",
                "podman",
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
    fn a_build_is_named_before_the_engine_arguments() {
        let wrap = wrap(
            "podman",
            vec![Build {
                image: "localhost/moebius-container-sandbox:ab".into(),
                dir: "/tmp/n-build".into(),
                base: None,
            }],
            &["run"],
        );
        assert_eq!(
            wrap.to_args(),
            [
                "run",
                "--engine",
                "podman",
                "podman",
                "--pass",
                "",
                "--build",
                "localhost/moebius-container-sandbox:ab",
                "/tmp/n-build",
                "--",
                "run"
            ]
        );
    }

    #[test]
    fn what_to_args_writes_parse_reads() {
        let wrap = Wrap {
            engine: Engine::Docker,
            program: "/usr/bin/docker".into(),
            pass: vec!["MOEBIUS_RUN_TOKEN".into()],
            builds: vec![
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
            ],
            own: Some(Own {
                image: "repo:2".into(),
                user: User {
                    uid: 1000,
                    gid: 1001,
                },
                volumes: vec!["npm".into(), "cargo".into()],
            }),
            relay: Some(Relay {
                dir: "/tmp/n-relay".into(),
                ports: vec![7172, 7173],
            }),
            args: vec!["run".into(), "--".into(), "x".into()],
        };
        let args = wrap.to_args();
        assert_eq!(
            args[6..],
            [
                "--build",
                "base:1",
                "/tmp/n-0-build",
                "--build",
                "repo:2",
                "/tmp/n-1-build",
                "--base",
                "base:1",
                "--own",
                "repo:2",
                "1000:1001",
                "npm,cargo",
                "--relay",
                "/tmp/n-relay",
                "7172,7173",
                "--",
                "run",
                "--",
                "x"
            ]
        );
        assert_eq!(Wrap::parse(&args[1..]), Some(wrap));
        // Anything else than options and the `--` that ends them is no subcommand.
        let start = ["--engine", "podman", "podman", "--pass", ""].map(String::from);
        for tail in [
            &["--build", "x"][..],
            &["--base", "x", "--"],
            &["--relay", "/d", "x", "--"],
        ] {
            let args: Vec<String> = start
                .iter()
                .cloned()
                .chain(tail.iter().map(|a| a.to_string()))
                .collect();
            assert_eq!(Wrap::parse(&args), None, "{tail:?}");
        }
        let lxc: Vec<String> = ["--engine", "lxc", "lxc", "--pass", "", "--"]
            .map(String::from)
            .into();
        assert_eq!(Wrap::parse(&lxc), None);
    }

    #[test]
    fn the_owning_container_can_only_change_owners_of_the_volumes() {
        let args = own_args(&Own {
            image: "localhost/shop".into(),
            user: User {
                uid: 1000,
                gid: 1001,
            },
            volumes: vec!["npm".into(), "cargo".into()],
        });
        assert_eq!(
            args.join(" "),
            "run --rm --pull never --log-driver none --network none --user 0:0 --cap-drop ALL --cap-add CHOWN \
             --security-opt no-new-privileges --label moebius.plugin=container-sandbox \
             --volume npm:/moebius-volumes/0:z --volume cargo:/moebius-volumes/1:z --entrypoint chown \
             localhost/shop 1000:1001 /moebius-volumes/0 /moebius-volumes/1"
        );
    }

    #[test]
    fn each_engine_removes_at_once() {
        assert_eq!(
            remove_args(Engine::Podman, "c"),
            ["rm", "--force", "--time", "0", "--ignore", "c"]
        );
        assert_eq!(remove_args(Engine::Docker, "c"), ["rm", "--force", "c"]);
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
            let wrap = wrap(
                &podman.to_string_lossy(),
                vec![Build {
                    image: "localhost/moebius-container-sandbox:ab".into(),
                    dir: context.clone(),
                    base: None,
                }],
                &["run", "--rm"],
            );
            let outcome = run(&wrap.to_args()).unwrap();
            (outcome, calls(&log))
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
                "image exists localhost/moebius-container-sandbox:ab".to_string(),
                format!(
                    "build --tag localhost/moebius-container-sandbox:ab --label moebius.plugin=container-sandbox --file {} {}",
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
                "image exists localhost/moebius-container-sandbox:ab",
                "run --rm"
            ]
        );
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
        let builds = vec![
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
        let wrap = wrap(&podman.to_string_lossy(), builds, &["run"]);
        std::fs::write(&log, "").unwrap();
        let outcome = run(&wrap.to_args()).unwrap();
        assert_eq!(outcome.code, 0, "{outcome:?}");
        let build_call = |image: &str, extra: &str, name: &str| {
            format!(
                "build --tag {image} --label moebius.plugin=container-sandbox {extra}--file {} {}",
                dir.path().join(name).join("Dockerfile").display(),
                dir.path().join(name).display()
            )
        };
        assert_eq!(
            calls(&log),
            [
                "image exists base".to_string(),
                build_call("base", "", "base-build"),
                "image exists repo".to_string(),
                build_call("repo", "--build-arg MOEBIUS_BASE=base ", "repo-build"),
                "run".to_string(),
            ]
        );
    }

    #[cfg(unix)] // The fake Docker is a shell script.
    #[test]
    fn a_docker_run_inspects_for_its_image_owns_its_volumes_and_relays_while_the_container_runs() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log");
        let docker = dir.path().join("docker");
        let relay = dir.path().join("relay");
        std::fs::create_dir(&relay).unwrap();
        // The container's run says whether the relay's socket is there while it runs.
        let text = format!(
            "#!/bin/sh\necho \"$@\" >> {log}\ncase \"$*\" in\n  image*) exit 1 ;;\n  *--entrypoint*) exit 0 ;;\n  run*) [ -S {socket} ] && echo socket >> {log} ;;\nesac\nexit 0\n",
            log = log.display(),
            socket = relay.join("7172").display(),
        );
        crate::testing::script(&docker, &text);
        let context = dir.path().join("n-build");
        std::fs::create_dir(&context).unwrap();
        std::fs::write(context.join("Dockerfile"), "FROM debian\n").unwrap();
        std::fs::write(&log, "").unwrap();
        let own = Own {
            image: "repo".into(),
            user: User {
                uid: 1000,
                gid: 1001,
            },
            volumes: vec!["npm".into()],
        };
        let wrap = Wrap {
            engine: Engine::Docker,
            program: docker.to_string_lossy().into_owned(),
            pass: Vec::new(),
            builds: vec![Build {
                image: "repo".into(),
                dir: context.clone(),
                base: None,
            }],
            own: Some(own.clone()),
            relay: Some(Relay {
                dir: relay.clone(),
                ports: vec![7172],
            }),
            args: vec!["run".into(), "--rm".into()],
        };
        let outcome = run(&wrap.to_args()).unwrap();
        assert_eq!(outcome.code, 0, "{outcome:?}");
        let calls = calls(&log);
        assert_eq!(calls[0], "image inspect --format {{.Id}} repo");
        assert!(calls[1].starts_with("build --tag repo"), "{calls:?}");
        assert_eq!(calls[2], own_args(&own).join(" "));
        assert_eq!(calls[3..], ["run --rm", "socket"]);
    }

    #[cfg(unix)] // The fake Docker is a shell script.
    #[test]
    fn a_docker_run_whose_volumes_cannot_be_owned_never_starts_its_container() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log");
        let docker = dir.path().join("docker");
        let text = format!(
            "#!/bin/sh\necho \"$@\" >> {log}\ncase \"$*\" in\n  *--entrypoint*) exit 125 ;;\nesac\nexit 0\n",
            log = log.display(),
        );
        crate::testing::script(&docker, &text);
        std::fs::write(&log, "").unwrap();
        let wrap = Wrap {
            engine: Engine::Docker,
            program: docker.to_string_lossy().into_owned(),
            pass: Vec::new(),
            builds: Vec::new(),
            own: Some(Own {
                image: "repo".into(),
                user: User { uid: 1, gid: 1 },
                volumes: vec!["npm".into()],
            }),
            relay: None,
            args: vec!["run".into()],
        };
        let outcome = run(&wrap.to_args()).unwrap();
        assert_eq!(outcome.code, 1);
        assert!(
            outcome.said.contains("cannot give the volumes npm"),
            "{outcome:?}"
        );
        assert_eq!(calls(&log).len(), 1, "no container: {:?}", calls(&log));
    }

    #[cfg(unix)] // The command is a shell.
    #[test]
    fn the_relay_runs_its_command_and_exits_as_it_did() {
        let dir = tempfile::tempdir().unwrap();
        let args: Vec<String> = [
            "relay",
            &dir.path().to_string_lossy(),
            "",
            "--",
            "/bin/sh",
            "-c",
            "exit 4",
        ]
        .map(String::from)
        .into();
        assert_eq!(run(&args).unwrap().code, 4);
        let none: Vec<String> = ["relay", "/d", "", "--"].map(String::from).into();
        assert!(run(&none).unwrap().said.contains("no command"));
        let bad: Vec<String> = ["relay", "/d", "x", "--", "true"].map(String::from).into();
        assert_eq!(run(&bad), None);
    }

    #[test]
    fn ports_are_a_comma_separated_list() {
        assert_eq!(port_list("7172,7173"), Some(vec![7172, 7173]));
        assert_eq!(port_list(""), Some(vec![]));
        assert_eq!(port_list("7172,x"), None);
        assert_eq!(port_list("70000"), None);
    }

    #[test]
    fn arguments_that_are_no_subcommand_are_settings() {
        assert_eq!(run(&["--network".into(), "internet".into()]), None);
        assert_eq!(run(&["run".into(), "x".into()]), None);
        assert_eq!(run(&[]), None);
    }

    #[test]
    fn stop_leaves_other_containers_alone() {
        let said = run(&[
            "stop".into(),
            "podman".into(),
            "/does/not/exist".into(),
            "jig-shop".into(),
        ])
        .unwrap();
        assert_eq!(said.code, 0);
        assert!(said.said.contains("left alone"), "{said:?}");
    }

    #[cfg(unix)] // The fake engines are shell scripts.
    #[test]
    fn stop_asks_the_engine_and_a_failing_engine_does_not_fail_the_command() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log");
        let engine = dir.path().join("engine");
        let text = format!(
            "#!/bin/sh\necho \"$@\" > {}\necho 'Error response from daemon: No such container: x' >&2\nexit 125\n",
            log.display()
        );
        crate::testing::script(&engine, &text);
        let stop = |name: &str, program: &str| {
            run(&[
                "stop".into(),
                name.into(),
                program.into(),
                "moebius-run-7-abc".into(),
            ])
            .unwrap()
        };
        let said = stop("podman", &engine.to_string_lossy());
        assert_eq!(said.code, 0);
        assert_eq!(
            std::fs::read_to_string(&log).unwrap().trim(),
            "rm --force --time 0 --ignore moebius-run-7-abc"
        );
        assert!(said.said.contains("125"), "{said:?}");
        // Docker says a container already gone is gone; that is what the stop command wants.
        let said = stop("docker", &engine.to_string_lossy());
        assert_eq!(
            std::fs::read_to_string(&log).unwrap().trim(),
            "rm --force moebius-run-7-abc"
        );
        assert_eq!(
            said,
            Outcome {
                code: 0,
                said: String::new()
            }
        );
        let said = stop("podman", "/does/not/exist");
        assert!(said.said.contains("cannot run"), "{said:?}");
    }
}
