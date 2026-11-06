//! Runs small local commands through a real nono, with the profile the plugin writes. It runs only when
//! `MOEBIUS_E2E_NONO` is set, on Linux: `1` uses the `nono` on `PATH`, any other value is the path of the program.
//! CI installs the pinned nono and sets it. The tests check what the sandbox is for: a run reads and writes its
//! worktree and its temporary directory and writes nowhere else (not `/tmp`, not the data directory, not the profiles
//! directory), cannot read a file outside it, another worktree or the data directory, cannot connect to a Unix socket,
//! reaches the router's port but no other port in mode `moebius` (not on a non-loopback address either), reaches other
//! ports in mode `internet`, and has no provider key in its environment. The world lives under the target directory,
//! not under `/tmp`, which the sandbox must not grant.

#![cfg(target_os = "linux")]

use std::collections::BTreeMap;
use std::net::{IpAddr, TcpListener, UdpSocket};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use moebius_plugin_api::{
    Call, Kind, LaunchAnswer, LaunchRequest, LaunchRun, Plugin, Urls, Wrapped,
};
use nono_sandbox::launch::NonoSandbox;
use nono_sandbox::settings::{Network, Settings, UnixSockets};

/// The nono to test, or `None` when the test is not asked for.
fn nono() -> Option<String> {
    let value = std::env::var("MOEBIUS_E2E_NONO")
        .ok()
        .filter(|v| !v.is_empty())?;
    Some(if value == "1" || value == "true" {
        "nono".into()
    } else {
        value
    })
}

struct World {
    _dir: tempfile::TempDir,
    root: PathBuf,
    _listeners: Vec<TcpListener>,
    _socket: UnixListener,
    router: u16,
    other: u16,
    /// This machine's address on its network, with a listener on a port of its own and one on the router's port.
    external: Option<External>,
}

struct External {
    ip: IpAddr,
    other: u16,
    router: u16,
}

/// The address other machines would reach this one by, when it has one: the source address of a route to a
/// documentation address (nothing is sent).
fn external_ip() -> Option<IpAddr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_loopback() && !ip.is_unspecified()).then_some(ip)
}

fn has_python() -> bool {
    Command::new("python3").arg("--version").output().is_ok()
}

fn world() -> World {
    // Not under /tmp: the sandbox must not be able to write there, and a world there would hide a grant of it.
    let base = Path::new(env!("CARGO_TARGET_TMPDIR"));
    std::fs::create_dir_all(base).unwrap();
    let dir = tempfile::tempdir_in(base).unwrap();
    let root = dir.path().canonicalize().unwrap();
    assert!(
        !root.starts_with("/tmp"),
        "{root:?} must be outside /tmp to test it"
    );
    for sub in [
        "worktree",
        "worktree2",
        "run/tmp",
        "agent",
        "data",
        "profiles",
    ] {
        std::fs::create_dir_all(root.join(sub)).unwrap();
    }
    std::fs::write(root.join("worktree/in.txt"), "inside").unwrap();
    std::fs::write(root.join("worktree2/in.txt"), "another run").unwrap();
    std::fs::write(root.join("outside.txt"), "outside").unwrap();
    std::fs::write(root.join("data/secrets.toml"), "key = \"x\"").unwrap();
    std::fs::write(root.join("profiles/run-9-victim.json"), "{}").unwrap();
    let router = TcpListener::bind("127.0.0.1:0").unwrap();
    let other = TcpListener::bind("127.0.0.1:0").unwrap();
    let socket = UnixListener::bind(root.join("data/s.sock")).unwrap();
    let mut listeners = Vec::new();
    let router_port = router.local_addr().unwrap().port();
    let external = external_ip().and_then(|ip| {
        let other = TcpListener::bind((ip, 0)).ok()?;
        let same = TcpListener::bind((ip, router_port)).ok()?;
        let external = External {
            ip,
            other: other.local_addr().ok()?.port(),
            router: router_port,
        };
        listeners.extend([other, same]);
        Some(external)
    });
    listeners.extend([router, other]);
    let other_port = listeners.last().unwrap().local_addr().unwrap().port();
    World {
        router: router_port,
        other: other_port,
        _listeners: listeners,
        _socket: socket,
        external,
        root,
        _dir: dir,
    }
}

/// Settings left as they are.
fn unchanged(_: &mut Settings) {}

/// What the plugin answers for a run of `program` with `args` in `world`, in mode `mode`, with the settings after
/// `tweak` has changed them.
fn launch(
    world: &World,
    mode: Network,
    nono: &str,
    tweak: &dyn Fn(&mut Settings),
    program: &str,
    args: Vec<String>,
) -> Wrapped {
    let root = &world.root;
    let url = format!("http://127.0.0.1:{}", world.router);
    let request = LaunchRequest {
        run: LaunchRun {
            id: 1,
            purpose: "code".into(),
            agent: "Coder".into(),
            chat: false,
            card: Some(1),
        },
        program: program.into(),
        args,
        env: BTreeMap::from([
            (
                "PATH".to_string(),
                "/usr/local/bin:/usr/bin:/bin".to_string(),
            ),
            (
                "HOME".to_string(),
                root.join("run").to_string_lossy().into_owned(),
            ),
            (
                "TMPDIR".to_string(),
                root.join("run/tmp").to_string_lossy().into_owned(),
            ),
            (
                "MOEBIUS_RUN_TOKEN".to_string(),
                "moebius-run-e2e".to_string(),
            ),
            ("OPENAI_API_KEY".to_string(), "sk-e2e".to_string()),
        ]),
        dir: root.join("worktree").to_string_lossy().into_owned(),
        read_only: vec![root.join("agent").to_string_lossy().into_owned()],
        read_write: vec![
            root.join("worktree").to_string_lossy().into_owned(),
            root.join("run/tmp").to_string_lossy().into_owned(),
        ],
        urls: Urls {
            router: url.clone(),
            gateway: format!("{url}/mcp"),
            check: url,
        },
        clone: None,
        runner: None,
        repository: None,
        project_settings: BTreeMap::new(),
        app_settings: BTreeMap::new(),
    };
    let mut settings = Settings {
        nono: nono.to_string(),
        network: mode,
        profiles_dir: root.join("profiles"),
        ..Settings::default()
    };
    tweak(&mut settings);
    let plugin = NonoSandbox::new(settings, PathBuf::from(env!("CARGO_BIN_EXE_nono-sandbox")));
    let answer = plugin
        .call(Call::new(
            Kind::Launcher,
            "launch",
            serde_json::to_value(&request).unwrap(),
        ))
        .unwrap();
    let LaunchAnswer::Wrapped(wrapped) = serde_json::from_value(answer).unwrap() else {
        panic!("not wrapped")
    };
    *wrapped
}

/// Runs the check script in the sandbox the plugin makes for mode `mode` and the settings after `tweak`; returns its
/// `name=result` lines.
fn run(
    world: &World,
    mode: Network,
    nono: &str,
    tweak: &dyn Fn(&mut Settings),
) -> BTreeMap<String, String> {
    let root = &world.root;
    let python = has_python();
    let external = match &world.external {
        Some(e) => format!(
            "(exec 3<>/dev/tcp/{ip}/{other}) 2>/dev/null && echo connect_external_other=ok || echo connect_external_other=denied\n\
             (exec 3<>/dev/tcp/{ip}/{router}) 2>/dev/null && echo connect_external_router_port=ok || echo connect_external_router_port=denied\n",
            ip = e.ip,
            other = e.other,
            router = e.router,
        ),
        None => String::new(),
    };
    let python_checks = if python {
        let bind = world.external.as_ref().map_or(String::new(), |e| {
            format!(
                "python3 -c 'import socket,sys; s=socket.socket(); s.bind((sys.argv[1],0)); s.listen(1)' {} 2>/dev/null && echo bind_external=ok || echo bind_external=denied\n",
                e.ip
            )
        });
        format!(
            "python3 -c 'import socket,sys; s=socket.socket(socket.AF_UNIX); s.settimeout(5); s.connect(sys.argv[1])' '{root}/data/s.sock' 2>/dev/null && echo unix_socket=ok || echo unix_socket=denied\n{bind}",
            root = world.root.display(),
        )
    } else {
        String::new()
    };
    let script = format!(
        r#"
        cat '{root}/worktree/in.txt' >/dev/null 2>&1 && echo read_worktree=ok || echo read_worktree=denied
        echo x > '{root}/worktree/out.txt' 2>/dev/null && echo write_worktree=ok || echo write_worktree=denied
        echo x > "$TMPDIR/t" 2>/dev/null && echo write_tmp=ok || echo write_tmp=denied
        echo x > '{root}/outside-new.txt' 2>/dev/null && echo write_outside=ok || echo write_outside=denied
        echo x > '{root}/data/secrets.toml' 2>/dev/null && echo write_data=ok || echo write_data=denied
        echo x > '{root}/data/planted.toml' 2>/dev/null && echo write_data_new=ok || echo write_data_new=denied
        echo x > '{root}/profiles/run-9-victim.json' 2>/dev/null && echo write_profiles=ok || echo write_profiles=denied
        echo x > '{root}/profiles/run-8-planted.json' 2>/dev/null && echo write_profiles_new=ok || echo write_profiles_new=denied
        shared="/tmp/moebius-e2e-$$"
        echo x > "$shared" 2>/dev/null && {{ echo write_shared_tmp=ok; rm -f "$shared"; }} || echo write_shared_tmp=denied
        cat '{root}/outside.txt' >/dev/null 2>&1 && echo read_outside=ok || echo read_outside=denied
        cat '{root}/worktree2/in.txt' >/dev/null 2>&1 && echo read_other_worktree=ok || echo read_other_worktree=denied
        cat '{root}/data/secrets.toml' >/dev/null 2>&1 && echo read_secrets=ok || echo read_secrets=denied
        echo x > /dev/null && echo write_dev_null=ok || echo write_dev_null=denied
        (exec 3<>/dev/tcp/127.0.0.1/{router}) 2>/dev/null && echo connect_router=ok || echo connect_router=denied
        (exec 3<>/dev/tcp/127.0.0.1/{other}) 2>/dev/null && echo connect_other=ok || echo connect_other=denied
        [ -z "${{OPENAI_API_KEY:-}}" ] && echo provider_key=absent || echo provider_key=present
        [ -n "${{MOEBIUS_RUN_TOKEN:-}}" ] && echo run_token=present || echo run_token=absent
        {external}{python_checks}"#,
        root = root.display(),
        router = world.router,
        other = world.other,
    );
    let Wrapped {
        program,
        args,
        env,
        cleanup,
        remove,
        ..
    } = launch(world, mode, nono, tweak, "bash", vec!["-c".into(), script]);
    let output = Command::new(program)
        .args(args)
        .env_clear()
        .envs(env)
        .current_dir(root.join("worktree"))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    // Its cleanup command removes the profile.
    let cleanup = cleanup.unwrap();
    assert!(
        Command::new(cleanup.program)
            .args(cleanup.args)
            .status()
            .unwrap()
            .success()
    );
    assert!(!Path::new(&remove[0]).exists());

    let results: BTreeMap<String, String> = stdout
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    assert!(
        !results.is_empty(),
        "the sandboxed script printed nothing: status {}, stderr {stderr}",
        output.status
    );
    results
}

/// The results a run in mode `moebius` must produce, whatever the machine has.
fn expected_in_mode_moebius(world: &World) -> BTreeMap<String, String> {
    let mut expected: Vec<(&str, &str)> = vec![
        ("read_worktree", "ok"),
        ("write_worktree", "ok"),
        ("write_tmp", "ok"),
        ("write_dev_null", "ok"),
        ("write_outside", "denied"),
        ("write_data", "denied"),
        ("write_data_new", "denied"),
        ("write_profiles", "denied"),
        ("write_profiles_new", "denied"),
        ("write_shared_tmp", "denied"),
        ("read_outside", "denied"),
        ("read_other_worktree", "denied"),
        ("read_secrets", "denied"),
        ("connect_router", "ok"),
        ("connect_other", "denied"),
        ("provider_key", "absent"),
        ("run_token", "present"),
    ];
    if world.external.is_some() {
        expected.push(("connect_external_other", "denied"));
        // Landlock filters TCP by port, not by host: see `the_router_port_is_open_on_every_host_on_linux`.
        expected.push(("connect_external_router_port", "ok"));
    }
    if has_python() {
        expected.push(("unix_socket", "denied"));
        if world.external.is_some() {
            expected.push(("bind_external", "denied"));
        }
    }
    expected
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[test]
fn a_run_sees_its_worktree_and_the_router_and_nothing_else() {
    let Some(nono) = nono() else { return };
    let world = world();
    let results = run(&world, Network::Moebius, &nono, &unchanged);
    assert_eq!(results, expected_in_mode_moebius(&world));
}

#[test]
fn the_router_port_is_open_on_every_host_on_linux() {
    // The known limit of mode "moebius": nono allows a TCP port for every address, so the router's port reaches a
    // non-loopback address of this machine too. The test records it; the plugin's note and README say it. If nono
    // learns to filter by host, this test fails and the limit can go.
    let Some(nono) = nono() else { return };
    let world = world();
    if world.external.is_none() {
        eprintln!("this machine has no non-loopback address; nothing to record");
        return;
    }
    let results = run(&world, Network::Moebius, &nono, &unchanged);
    assert_eq!(results["connect_external_router_port"], "ok");
    assert_eq!(results["connect_external_other"], "denied");
}

#[test]
fn mode_internet_reaches_other_ports_but_the_files_stay_out_of_reach() {
    let Some(nono) = nono() else { return };
    let results = run(&world(), Network::Internet, &nono, &unchanged);
    assert_eq!(results["connect_router"], "ok");
    assert_eq!(results["connect_other"], "ok");
    for denied in [
        "read_outside",
        "read_other_worktree",
        "read_secrets",
        "write_outside",
        "write_data",
        "write_profiles",
        "write_shared_tmp",
    ] {
        assert_eq!(results[denied], "denied", "{denied}");
    }
    assert_eq!(results["provider_key"], "absent");
    if has_python() {
        assert_eq!(results["unix_socket"], "denied");
    }
}

#[test]
fn the_resolved_profile_has_none_of_the_groups_that_write_to_tmp() {
    // Asserted on what nono resolves from the file, not on the plugin's own JSON: nono adds base groups to every
    // profile whether the profile names them or not.
    let Some(nono) = nono() else { return };
    let world = world();
    let wrapped = launch(&world, Network::Moebius, &nono, &unchanged, "true", vec![]);
    let shown = Command::new(&nono)
        .args(["profile", "show"])
        .arg(&wrapped.remove[0])
        .output()
        .unwrap();
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&shown.stdout),
        String::from_utf8_lossy(&shown.stderr)
    );
    assert!(shown.status.success(), "nono profile show failed: {text}");
    // nono lists the groups it applies, then the ones the profile excludes: only the first list counts.
    let excluded = text
        .find("Excluded security groups:")
        .expect("nono lists the excluded groups");
    assert!(
        text[excluded..].contains("system_write_linux"),
        "the profile does not exclude the write groups:\n{text}"
    );
    let applied = &text[..excluded];
    for group in [
        "system_write_linux",
        "system_write_macos",
        "user_tools",
        "homebrew",
    ] {
        assert!(
            !applied.contains(group),
            "{group} in the resolved profile:\n{text}"
        );
    }
}

#[test]
fn allowed_unix_sockets_open_only_the_sockets() {
    // Without mediation a run reaches the socket (Landlock does not gate connect() to pathname sockets); every other
    // limit stays.
    let Some(nono) = nono() else { return };
    if !has_python() {
        eprintln!("python3 is needed to connect to the socket");
        return;
    }
    let world = world();
    let allow = |settings: &mut Settings| settings.unix_sockets = UnixSockets::Allow;
    let results = run(&world, Network::Moebius, &nono, &allow);
    let mut expected = expected_in_mode_moebius(&world);
    expected.insert("unix_socket".into(), "ok".into());
    assert_eq!(results, expected);
    for (name, value) in [
        ("read_outside", "denied"),
        ("read_other_worktree", "denied"),
        ("read_secrets", "denied"),
        ("write_outside", "denied"),
        ("connect_router", "ok"),
        ("connect_other", "denied"),
        ("provider_key", "absent"),
    ] {
        assert_eq!(results[name], value, "{name}");
    }
}

/// Starts the run with piped streams, as the core does.
fn spawn(wrapped: &Wrapped, world: &World) -> std::process::Child {
    Command::new(&wrapped.program)
        .args(&wrapped.args)
        .env_clear()
        .envs(&wrapped.env)
        .current_dir(world.root.join("worktree"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

#[test]
fn the_harness_keeps_its_standard_input_and_output() {
    use std::io::{BufRead, BufReader, Write};
    let Some(nono) = nono() else { return };
    let world = world();
    let wrapped = launch(&world, Network::Moebius, &nono, &unchanged, "cat", vec![]);
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

/// Whether the process `pid` is gone (a zombie that nobody has waited for yet counts as gone).
fn gone(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Err(_) => true,
        Ok(stat) => stat
            .rsplit_once(')')
            .is_some_and(|(_, rest)| rest.trim_start().starts_with('Z')),
    }
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !done() {
        assert!(std::time::Instant::now() < deadline, "timed out: {what}");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[test]
fn the_stop_command_succeeds_and_the_sandboxed_program_ends_with_nono() {
    let Some(nono) = nono() else { return };
    for signal in ["-TERM", "-KILL"] {
        let world = world();
        let pid_file = world.root.join("worktree/pid");
        // The program writes its own pid, then becomes `sleep`, so the test can see that the program itself ends.
        let wrapped = launch(
            &world,
            Network::Moebius,
            &nono,
            &unchanged,
            "sh",
            vec![
                "-c".into(),
                format!("echo $$ > '{}'; exec sleep 600", pid_file.display()),
            ],
        );
        let stop = wrapped.stop.as_ref().unwrap();
        // `nono stop` knows only the sessions nono manages (detached ones), so for this one it finds nothing; the
        // command still succeeds, and it succeeds again, since the core runs it more than once.
        for _ in 0..2 {
            assert!(
                Command::new(&stop.program)
                    .args(&stop.args)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let mut child = spawn(&wrapped, &world);
        let mut pid = None;
        wait_until("the sandboxed program writes its pid", || {
            pid = std::fs::read_to_string(&pid_file)
                .ok()
                .and_then(|text| text.trim().parse::<u32>().ok());
            pid.is_some()
        });
        let pid = pid.unwrap();
        assert!(!gone(pid), "the program runs before the signal");
        // The core kills the wrapper's process group; here nono alone is signalled, which is the harder case.
        let status = Command::new("kill")
            .args([signal, &child.id().to_string()])
            .status()
            .unwrap();
        assert!(status.success());
        child.wait().unwrap();
        wait_until(
            &format!("the sandboxed program ends after {signal} to nono"),
            || gone(pid),
        );
    }
}
