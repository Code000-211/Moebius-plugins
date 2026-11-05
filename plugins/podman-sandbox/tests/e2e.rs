//! Runs small scripts through a real rootless Podman, in the container the plugin describes. It runs only when
//! `MOEBIUS_E2E_PODMAN` is set, on Linux: `1` uses the `podman` on `PATH`, any other value is the path of the
//! program. The image is `MOEBIUS_E2E_PODMAN_IMAGE`, `docker.io/library/debian:stable-slim` when it is not set; it
//! must be pulled already (the plugin never pulls), and needs `bash` and `timeout`. The tests check what the sandbox
//! is for: a run reads and writes its worktree and temporary directory as the user, reads its read-only paths without
//! writing them, sees nothing else of the host (not the data directory, its secrets, another worktree or this
//! repository), has its token and no provider key, reaches the router's port but no other port of the host or of
//! another machine in mode `moebius`, keeps its standard input and output, ends with the stop command, and that a
//! container left behind is named by `leftovers`.

#![cfg(target_os = "linux")]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, TcpListener, UdpSocket};
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use moebius_plugin_api::{
    LaunchAnswer, LaunchRequest, LaunchRun, LeftoversRequest, Plugin, Urls, Wrapped,
};
use podman_sandbox::launch::PodmanSandbox;
use podman_sandbox::settings::{Network, Settings};

mod common;

/// The Podman to test, or `None` when the test is not asked for.
fn podman() -> Option<String> {
    let value = std::env::var("MOEBIUS_E2E_PODMAN")
        .ok()
        .filter(|v| !v.is_empty())?;
    let program = if value == "1" || value == "true" {
        "podman".into()
    } else {
        value
    };
    Some(podman_sandbox::check::resolve(&program).unwrap_or(program))
}

fn image() -> String {
    std::env::var("MOEBIUS_E2E_PODMAN_IMAGE")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "docker.io/library/debian:stable-slim".into())
}

/// The address other machines would reach this one by, when it has one: the source address of a route to a
/// documentation address (nothing is sent).
fn external_ip() -> Option<IpAddr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_loopback() && !ip.is_unspecified()).then_some(ip)
}

struct World {
    _dir: tempfile::TempDir,
    root: PathBuf,
    _listeners: Vec<TcpListener>,
    router: u16,
    other: u16,
    /// This machine's address on its network, with a listener on a port of its own.
    external: Option<(IpAddr, u16)>,
}

/// A data directory with a run's worktree and another's, a secret, a clone, a read-only file and the harness's
/// installation. Under the target directory, whose path is the same for the host's Podman as for this test.
fn world() -> World {
    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let root = dir.path().canonicalize().unwrap();
    for sub in [
        "data/worktrees/run-7",
        "data/worktrees/run-8",
        "data/runs/7/tmp",
        "data/repos/shop",
        "harness",
    ] {
        std::fs::create_dir_all(root.join(sub)).unwrap();
    }
    std::fs::write(root.join("data/secrets.toml"), "token = 'secret'").unwrap();
    std::fs::write(root.join("data/worktrees/run-8/theirs"), "x").unwrap();
    std::fs::write(root.join("data/repos/shop/README"), "shop").unwrap();
    std::fs::write(root.join("data/runs/7/bridge.ts"), "bridge").unwrap();
    let listen = |ip: &str| {
        let listener = TcpListener::bind((ip, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        (listener, port)
    };
    let (router_listener, router) = listen("127.0.0.1");
    let (other_listener, other) = listen("127.0.0.1");
    let mut listeners = vec![router_listener, other_listener];
    let external = external_ip().map(|ip| {
        let (listener, port) = listen(&ip.to_string());
        listeners.push(listener);
        (ip, port)
    });
    World {
        _dir: dir,
        root,
        _listeners: listeners,
        router,
        other,
        external,
    }
}

impl World {
    fn at(&self, path: &str) -> String {
        self.root.join(path).to_string_lossy().into_owned()
    }

    /// Writes the harness, a shell script of the image's `/bin/sh`, and returns its path. `script` runs it once with
    /// `--probe` to wait until it can be run, so it ends at once for that.
    fn harness(&self, body: &str) -> String {
        let path = self.root.join("harness/pi-bolt");
        common::script(
            &path,
            &format!("#!/bin/sh\n[ \"$1\" = --probe ] && exit 0\n{body}"),
        );
        path.to_string_lossy().into_owned()
    }

    fn request(&self, program: String) -> LaunchRequest {
        let router = format!("http://127.0.0.1:{}", self.router);
        LaunchRequest {
            run: LaunchRun {
                id: 7,
                purpose: "code".into(),
                agent: "Coder".into(),
                chat: false,
                card: Some(3),
            },
            program,
            args: vec!["--mode".into(), "rpc".into()],
            env: BTreeMap::from([
                ("PATH".to_string(), "/usr/bin:/bin".to_string()),
                (
                    "HOME".to_string(),
                    std::env::var("HOME").unwrap_or_default(),
                ),
                ("TMPDIR".to_string(), self.at("data/runs/7/tmp")),
                (
                    "MOEBIUS_RUN_TOKEN".to_string(),
                    "moebius-run-x y$z".to_string(),
                ),
                ("OPENAI_API_KEY".to_string(), "sk-provider".to_string()),
            ]),
            dir: self.at("data/worktrees/run-7"),
            read_only: vec![self.at("data/runs/7/bridge.ts")],
            read_write: vec![self.at("data/worktrees/run-7"), self.at("data/runs/7/tmp")],
            urls: Urls {
                router: router.clone(),
                gateway: format!("{router}/mcp"),
                check: router,
            },
            clone: Some(self.at("data/repos/shop")),
            runner: None,
        }
    }
}

fn plugin(podman: &str, network: Network) -> PodmanSandbox {
    let settings = Settings {
        podman: podman.into(),
        image: Some(image()),
        network,
        ..Settings::default()
    };
    PodmanSandbox::new(
        settings,
        PathBuf::from(env!("CARGO_BIN_EXE_podman-sandbox")),
        PodmanSandbox::podman_env_of(std::env::vars()),
    )
}

fn wrapped(plugin: &PodmanSandbox, request: LaunchRequest) -> Wrapped {
    let call = moebius_plugin_api::Call::new(
        moebius_plugin_api::Kind::Launcher,
        "launch",
        serde_json::to_value(request).unwrap(),
    );
    let answer: LaunchAnswer = serde_json::from_value(plugin.call(call).unwrap()).unwrap();
    match answer {
        LaunchAnswer::Wrapped(wrapped) => *wrapped,
        other => panic!("not wrapped: {other:?}"),
    }
}

/// Starts the wrapped launch as the core does: its environment only, in the worktree, in a process group of its own.
fn spawn(wrapped: &Wrapped, world: &World) -> Child {
    Command::new(&wrapped.program)
        .args(&wrapped.args)
        .env_clear()
        .envs(&wrapped.env)
        .current_dir(world.at("data/worktrees/run-7"))
        .process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

/// Runs the launch to its end and returns its output as `name=value` lines.
fn run(wrapped: &Wrapped, world: &World) -> BTreeMap<String, String> {
    let mut child = spawn(wrapped, world);
    drop(child.stdin.take());
    let mut stdout = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut stdout)
        .unwrap();
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    let status = child.wait().unwrap();
    assert!(status.success(), "{status}: {stdout}\n{stderr}");
    stdout
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// A shell line that prints `<name>=open` or `<name>=closed` for a TCP connection to `host`:`port`.
fn connect(name: &str, host: &str, port: u16) -> String {
    format!(
        "if timeout 10 bash -c 'exec 3<>/dev/tcp/{host}/{port}' 2>/dev/null; then echo {name}=open; else echo {name}=closed; fi\n"
    )
}

/// What the probe prints for whether it can do something: `yes` or `no`.
fn can(name: &str, command: &str) -> String {
    format!("if {command} 2>/dev/null; then echo {name}=yes; else echo {name}=no; fi\n")
}

fn probe(world: &World, network: Network) -> BTreeMap<String, String> {
    let Some(podman) = podman() else {
        return BTreeMap::new();
    };
    let data = world.at("data");
    let mut body = String::new();
    body.push_str("echo uid=$(id -u)\necho home=$HOME\necho args=\"$*\"\n");
    body.push_str("echo token=\"$MOEBIUS_RUN_TOKEN\"\necho key=\"$OPENAI_API_KEY\"\necho tmpdir=\"$TMPDIR\"\n");
    body.push_str(&can("write_worktree", "echo w > written"));
    body.push_str(&can("write_tmp", "echo w > \"$TMPDIR/written\""));
    body.push_str(&can("write_home", "echo w > \"$HOME/written\""));
    body.push_str(&can(
        "read_bridge",
        &format!("cat {data}/runs/7/bridge.ts >/dev/null"),
    ));
    body.push_str(&can(
        "write_bridge",
        &format!("echo w >> {data}/runs/7/bridge.ts"),
    ));
    body.push_str(&can(
        "read_clone",
        &format!("cat {data}/repos/shop/README >/dev/null"),
    ));
    body.push_str(&can(
        "write_clone",
        &format!("echo w > {data}/repos/shop/new"),
    ));
    body.push_str(&can(
        "read_secrets",
        &format!("cat {data}/secrets.toml >/dev/null"),
    ));
    body.push_str(&can(
        "see_other_run",
        &format!("ls {data}/worktrees/run-8/theirs >/dev/null"),
    ));
    body.push_str(&can(
        "see_this_repository",
        &format!("test -e {}/Cargo.toml", env!("CARGO_MANIFEST_DIR")),
    ));
    body.push_str(&connect("router", "127.0.0.1", world.router));
    body.push_str(&connect("other", "127.0.0.1", world.other));
    if let Some((ip, port)) = world.external {
        body.push_str(&connect("external", &ip.to_string(), port));
    }
    let program = world.harness(&body);
    let wrapped = wrapped(&plugin(&podman, network), world.request(program));
    run(&wrapped, world)
}

#[test]
fn a_run_sees_its_paths_and_the_router_and_nothing_else() {
    let Some(_) = podman() else { return };
    let world = world();
    let seen = probe(&world, Network::Moebius);
    let expected = [
        ("home", "/home/moebius"),
        ("args", "--mode rpc"),
        ("token", "moebius-run-x y$z"),
        ("key", ""),
        ("write_worktree", "yes"),
        ("write_tmp", "yes"),
        ("write_home", "yes"),
        ("read_bridge", "yes"),
        ("write_bridge", "no"),
        ("read_clone", "yes"),
        ("write_clone", "no"),
        ("read_secrets", "no"),
        ("see_other_run", "no"),
        ("see_this_repository", "no"),
        ("router", "open"),
        ("other", "closed"),
    ];
    for (name, value) in expected {
        assert_eq!(
            seen.get(name).map(String::as_str),
            Some(value),
            "{name} in {seen:?}"
        );
    }
    assert_eq!(seen.get("tmpdir"), Some(&world.at("data/runs/7/tmp")));
    if world.external.is_some() {
        assert_eq!(seen["external"], "closed", "{seen:?}");
    }
    // The run is the user: what it wrote in the worktree is the user's.
    let worktree = Path::new(&world.root).join("data/worktrees/run-7");
    let written = std::fs::metadata(worktree.join("written")).unwrap();
    assert_eq!(written.uid(), std::fs::metadata(&worktree).unwrap().uid());
    assert_eq!(seen["uid"], written.uid().to_string());
}

#[test]
fn mode_internet_still_reaches_no_other_port_of_the_host() {
    let Some(_) = podman() else { return };
    let world = world();
    let seen = probe(&world, Network::Internet);
    assert_eq!(seen["router"], "open", "{seen:?}");
    assert_eq!(seen["other"], "closed", "{seen:?}");
    assert_eq!(seen["read_secrets"], "no", "{seen:?}");
}

#[test]
fn the_harness_keeps_its_standard_input_and_output() {
    let Some(podman) = podman() else { return };
    let world = world();
    let program = world.harness("exec cat\n");
    let wrapped = wrapped(&plugin(&podman, Network::Moebius), world.request(program));
    let mut child = spawn(&wrapped, &world);
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    for line in ["{\"type\":\"prompt\"}", "second"] {
        writeln!(input, "{line}").unwrap();
        input.flush().unwrap();
        let mut echoed = String::new();
        output.read_line(&mut echoed).unwrap();
        assert_eq!(echoed.trim_end(), line);
    }
    drop(input);
    assert!(child.wait().unwrap().success());
}

fn exists(podman: &str, name: &str) -> bool {
    Command::new(podman)
        .args(["container", "exists", name])
        .status()
        .unwrap()
        .success()
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while !done() {
        assert!(std::time::Instant::now() < deadline, "timed out: {what}");
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Starts a launch whose harness says `ready` and sleeps, and waits for the word.
fn sleeping(podman: &str, world: &World) -> (Wrapped, Child) {
    let program = world.harness("echo ready\nexec sleep 600\n");
    let wrapped = wrapped(&plugin(podman, Network::Moebius), world.request(program));
    let mut child = spawn(&wrapped, world);
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut ready = String::new();
    output.read_line(&mut ready).unwrap();
    assert_eq!(ready.trim_end(), "ready");
    (wrapped, child)
}

fn run_command(command: &moebius_plugin_api::CommandLine) {
    let status = Command::new(&command.program)
        .args(&command.args)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", std::env::var("HOME").unwrap_or_default())
        .envs(&command.env)
        .status()
        .unwrap();
    assert!(status.success(), "{command:?}");
}

#[test]
fn the_stop_command_removes_the_container_and_the_launch_ends() {
    let Some(podman) = podman() else { return };
    let world = world();
    let (wrapped, mut child) = sleeping(&podman, &world);
    let stop = wrapped.stop.clone().unwrap();
    let name = stop.args[2].clone();
    assert!(exists(&podman, &name));
    // The core runs the stop command more than once for one process.
    run_command(&stop);
    run_command(&stop);
    wait_until("the launch ends", || child.try_wait().unwrap().is_some());
    assert!(!exists(&podman, &name));
}

#[test]
fn a_container_whose_launch_was_killed_is_a_leftover() {
    let Some(podman) = podman() else { return };
    let world = world();
    let (wrapped, mut child) = sleeping(&podman, &world);
    let name = wrapped.stop.as_ref().unwrap().args[2].clone();
    // The server died: its launch's process group is killed, and the container stays.
    let group = format!("-{}", child.id());
    assert!(
        Command::new("kill")
            .args(["-KILL", "--", &group])
            .status()
            .unwrap()
            .success()
    );
    child.wait().unwrap();
    assert!(exists(&podman, &name));

    let plugin = plugin(&podman, Network::Moebius);
    let still = plugin.call(moebius_plugin_api::Call::new(
        moebius_plugin_api::Kind::Launcher,
        "leftovers",
        serde_json::to_value(LeftoversRequest { running: vec![7] }).unwrap(),
    ));
    let still: moebius_plugin_api::Leftovers = serde_json::from_value(still.unwrap()).unwrap();
    assert!(
        !still.cleanup.iter().any(|c| c.args[2] == name),
        "a running run's container is not a leftover"
    );
    let left = plugin.call(moebius_plugin_api::Call::new(
        moebius_plugin_api::Kind::Launcher,
        "leftovers",
        serde_json::to_value(LeftoversRequest::default()).unwrap(),
    ));
    let left: moebius_plugin_api::Leftovers = serde_json::from_value(left.unwrap()).unwrap();
    let cleanup = left
        .cleanup
        .iter()
        .find(|c| c.args[2] == name)
        .unwrap_or_else(|| panic!("{name} is not among {left:?}"));
    run_command(cleanup);
    assert!(!exists(&podman, &name));
}
