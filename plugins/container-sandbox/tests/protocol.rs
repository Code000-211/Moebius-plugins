//! Runs the built plugin as a process and speaks the JSON Lines protocol to it, the way Moebius does, then starts the
//! wrapped launch it answers with, against a fake Podman. The Docker side of the wrapped launch is tested in the unit
//! tests of `commands`, since its relay would listen on the router's port of this machine.

mod common;

// The plugin starts on Linux only, and the fake Podman is a shell script.
#[cfg(target_os = "linux")]
mod linux {
    use std::io::{BufRead, BufReader, Write};
    use std::path::Path;
    use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

    use serde_json::{Value, json};

    use super::common::{fake_podman, script};

    struct Plugin {
        child: Child,
        input: ChildStdin,
        output: BufReader<ChildStdout>,
    }

    impl Plugin {
        fn start(args: &[&str]) -> Self {
            let mut child = Command::new(env!("CARGO_BIN_EXE_container-sandbox"))
                .args(args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let input = child.stdin.take().unwrap();
            let output = BufReader::new(child.stdout.take().unwrap());
            Self {
                child,
                input,
                output,
            }
        }

        fn line(&mut self) -> Value {
            let mut line = String::new();
            self.output.read_line(&mut line).unwrap();
            serde_json::from_str(&line).unwrap_or_else(|e| panic!("not JSON: {line:?}: {e}"))
        }

        fn call(&mut self, id: u64, method: &str, params: Value) -> Value {
            let call = json!({"type": "call", "id": id, "call": {"kind": "launcher", "method": method, "params": params}});
            writeln!(self.input, "{call}").unwrap();
            self.input.flush().unwrap();
            let reply = self.line();
            assert_eq!(reply["id"], id, "{reply}");
            reply
        }
    }

    fn launch_params(root: &Path, agent: &str, runner: Value) -> Value {
        let at = |p: &str| root.join(p).to_string_lossy().into_owned();
        json!({
            "run": {"id": 7, "purpose": "code", "agent": agent, "chat": false, "card": 3},
            "program": at("harness/pi-bolt"),
            "args": ["--mode", "rpc"],
            "env": {
                "PATH": "/usr/bin:/bin",
                "MOEBIUS_RUN_TOKEN": "moebius-run-x y$z`w`",
                "ANTHROPIC_API_KEY": "sk-x",
            },
            "dir": at("worktree"),
            "read_only": [],
            "read_write": [at("worktree")],
            "urls": {"router": "http://127.0.0.1:7172", "gateway": "http://127.0.0.1:7172/mcp", "check": "http://127.0.0.1:7172"},
            "clone": null,
            "runner": runner,
        })
    }

    fn strings(value: &Value) -> Vec<String> {
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn manifest_launch_the_wrapped_run_and_leftovers() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let podman = fake_podman(&root);
        std::fs::create_dir_all(root.join("worktree")).unwrap();
        std::fs::create_dir_all(root.join("harness")).unwrap();
        // The harness says what token it got, then echoes its standard input.
        script(
            &root.join("harness/pi-bolt"),
            "#!/bin/sh\nprintf 'token=[%s] key=[%s] args=%s\\n' \"$MOEBIUS_RUN_TOKEN\" \"$ANTHROPIC_API_KEY\" \"$*\"\nexec cat\n",
        );
        let mut plugin = Plugin::start(&[
            "--engine-program",
            podman.to_str().unwrap(),
            "--image",
            "localhost/base",
            "--exclude-agent",
            "Moebius",
        ]);

        let manifest = plugin.line();
        assert_eq!(manifest["type"], "manifest");
        assert_eq!(manifest["manifest"]["name"], "container-sandbox");
        assert_eq!(manifest["manifest"]["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(manifest["manifest"]["kinds"], json!(["launcher"]));
        // Each repository is asked for the Dockerfile of its image, in a code editor.
        let setting = &manifest["manifest"]["project_settings"][0];
        assert_eq!(setting["key"], "dockerfile");
        assert_eq!(setting["input"], "code");
        assert_eq!(setting["language"], "dockerfile");
        // The server is asked for the Dockerfile that theirs start from.
        let setting = &manifest["manifest"]["app_settings"][0];
        assert_eq!(setting["key"], "base_dockerfile");
        assert_eq!(setting["input"], "code");
        assert_eq!(setting["language"], "dockerfile");

        let reply = plugin.call(1, "launch", launch_params(&root, "Coder", Value::Null));
        assert_eq!(reply["type"], "reply", "{reply}");
        let result = &reply["result"];
        assert_eq!(result["answer"], "wrapped");
        assert_eq!(result["program"], env!("CARGO_BIN_EXE_container-sandbox"));
        assert!(result["env"].get("ANTHROPIC_API_KEY").is_none());
        assert!(!result["args"].to_string().contains("moebius-run-x"));

        // The wrapped launch, started as the core starts it: in its environment only, with Pi's protocol on its
        // standard input and output.
        let env: Vec<(String, String)> = result["env"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
            .collect();
        let mut child = Command::new(result["program"].as_str().unwrap())
            .args(strings(&result["args"]))
            .env_clear()
            .envs(env)
            .current_dir(root.join("worktree"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut input = child.stdin.take().unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap());
        let mut said = String::new();
        output.read_line(&mut said).unwrap();
        assert_eq!(
            said.trim_end(),
            "token=[moebius-run-x y$z`w`] key=[] args=--mode rpc"
        );
        writeln!(input, "{{\"type\":\"prompt\"}}").unwrap();
        input.flush().unwrap();
        let mut echoed = String::new();
        output.read_line(&mut echoed).unwrap();
        assert_eq!(echoed.trim_end(), "{\"type\":\"prompt\"}");
        drop(input);
        assert!(child.wait().unwrap().success());

        // Podman itself never had the token: not in its arguments, not in its environment.
        let podman_args = std::fs::read_to_string(root.join("run.args")).unwrap();
        let podman_env = std::fs::read_to_string(root.join("run.env")).unwrap();
        assert!(!podman_args.contains("moebius-run-x"), "{podman_args}");
        assert!(!podman_env.contains("moebius-run-x"), "{podman_env}");

        // The stop command succeeds, against a Podman that has nothing to remove.
        let stop = &result["stop"];
        let status = Command::new(stop["program"].as_str().unwrap())
            .args(strings(&stop["args"]))
            .status()
            .unwrap();
        assert!(status.success());

        // An excluded agent and a run on a runner are unchanged, with a note.
        for (id, request) in [
            (2, launch_params(&root, "Moebius", Value::Null)),
            (3, launch_params(&root, "Coder", json!("gpu-box"))),
        ] {
            let reply = plugin.call(id, "launch", request);
            assert_eq!(reply["result"]["answer"], "unchanged", "{reply}");
            assert!(reply["result"]["note"].is_string());
        }

        // A repository's Dockerfile: the wrapped launch builds its image from a directory that holds it alone,
        // which the core removes after the run.
        let mut params = launch_params(&root, "Coder", Value::Null);
        params["repository"] = json!("shop");
        params["project_settings"] = json!({"dockerfile": "FROM debian\n"});
        let reply = plugin.call(6, "launch", params);
        let args = strings(&reply["result"]["args"]);
        let at = args.iter().position(|a| a == "--build").expect("a build");
        assert!(args[at + 1].starts_with("localhost/moebius-container-sandbox:"));
        // The image is the one built and the one run.
        assert_eq!(args.iter().filter(|a| **a == args[at + 1]).count(), 2);
        let context = Path::new(&args[at + 2]);
        assert_eq!(
            std::fs::read_to_string(context.join("Dockerfile")).unwrap(),
            "FROM debian\n"
        );
        assert_eq!(std::fs::read_dir(context).unwrap().count(), 1);
        assert_eq!(reply["result"]["remove"], json!([args[at + 2]]));
        std::fs::remove_dir_all(context).unwrap();

        // With a base Dockerfile for the server too, two directories are built, the base first, and the repository's
        // image is built on it.
        let mut params = launch_params(&root, "Coder", Value::Null);
        params["repository"] = json!("shop");
        params["project_settings"] =
            json!({"dockerfile": "ARG MOEBIUS_BASE\nFROM ${MOEBIUS_BASE}\n"});
        params["app_settings"] = json!({"base_dockerfile": "FROM debian\n"});
        let reply = plugin.call(7, "launch", params);
        let args = strings(&reply["result"]["args"]);
        let builds: Vec<usize> = args
            .iter()
            .enumerate()
            .filter(|(_, a)| **a == "--build")
            .map(|(at, _)| at)
            .collect();
        assert_eq!(builds.len(), 2, "{args:?}");
        let (base, own) = (builds[0], builds[1]);
        assert_eq!(
            std::fs::read_to_string(Path::new(&args[base + 2]).join("Dockerfile")).unwrap(),
            "FROM debian\n"
        );
        assert_eq!(args[own + 3], "--base");
        assert_eq!(args[own + 4], args[base + 1]);
        assert_ne!(args[own + 1], args[base + 1]);
        assert_eq!(
            reply["result"]["remove"],
            json!([args[base + 2], args[own + 2]])
        );
        for dir in [&args[base + 2], &args[own + 2]] {
            std::fs::remove_dir_all(dir).unwrap();
        }

        let reply = plugin.call(4, "leftovers", json!({"running": []}));
        assert_eq!(reply["result"], json!({"cleanup": [], "remove": []}));

        // An error is an `error` line, not a crash.
        let reply = plugin.call(5, "decide", json!({}));
        assert_eq!(reply["type"], "error");

        // The plugin exits when its standard input closes.
        drop(plugin.input);
        assert!(plugin.child.wait().unwrap().success());
    }

    #[test]
    fn a_wrapped_launch_exits_as_its_container_did() {
        let dir = tempfile::tempdir().unwrap();
        let podman = dir.path().join("podman");
        script(&podman, "#!/bin/sh\ncat > /dev/null\nexit 3\n");
        let status = Command::new(env!("CARGO_BIN_EXE_container-sandbox"))
            .args([
                "run",
                "--engine",
                "podman",
                podman.to_str().unwrap(),
                "--pass",
                "",
                "--",
                "run",
            ])
            .stdin(Stdio::null())
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(3));
    }

    #[test]
    fn a_missing_image_stops_the_plugin_with_a_sentence() {
        let dir = tempfile::tempdir().unwrap();
        let podman = fake_podman(dir.path());
        let output = Command::new(env!("CARGO_BIN_EXE_container-sandbox"))
            .args([
                "--engine-program",
                podman.to_str().unwrap(),
                "--image",
                "localhost/absent",
            ])
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty(), "no manifest is written");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("localhost/absent"), "{stderr}");
    }

    #[test]
    fn a_docker_that_runs_rootless_stops_the_plugin_with_a_sentence() {
        let dir = tempfile::tempdir().unwrap();
        let docker = dir.path().join("docker");
        script(
            &docker,
            "#!/bin/sh\necho '{\"ServerVersion\":\"27.3.1\",\"OperatingSystem\":\"Ubuntu\",\"SecurityOptions\":[\"name=rootless\"]}'\n",
        );
        let output = Command::new(env!("CARGO_BIN_EXE_container-sandbox"))
            .args([
                "--engine",
                "docker",
                "--engine-program",
                docker.to_str().unwrap(),
            ])
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty(), "no manifest is written");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("rootless"), "{stderr}");
    }

    #[test]
    fn a_setting_that_is_wrong_stops_the_plugin_with_a_sentence() {
        let output = Command::new(env!("CARGO_BIN_EXE_container-sandbox"))
            .args(["--network", "wide-open"])
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("wide-open"));
    }
}

#[cfg(not(target_os = "linux"))]
#[test]
fn the_plugin_refuses_to_start_off_linux() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_container-sandbox"))
        .args(["--image", "base"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("only Linux"));
}
