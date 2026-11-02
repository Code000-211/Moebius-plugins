//! Runs the built plugin as a process and speaks the JSON Lines protocol to it, the way Moebius does.

mod common;

#[cfg(unix)] // The fake nono is a shell script.
mod unix {
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

    use serde_json::{Value, json};

    use super::common::fake_nono;

    struct Plugin {
        child: Child,
        input: ChildStdin,
        output: BufReader<ChildStdout>,
    }

    impl Plugin {
        fn start(args: &[&str]) -> Self {
            let mut child = Command::new(env!("CARGO_BIN_EXE_nono-sandbox"))
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

    fn launch_params(agent: &str, runner: Value) -> Value {
        json!({
            "run": {"id": 7, "purpose": "code", "agent": agent, "chat": false, "card": 3},
            "program": "pi-bolt",
            "args": ["--mode", "rpc"],
            "env": {"PATH": "/usr/bin", "MOEBIUS_RUN_TOKEN": "moebius-run-x", "ANTHROPIC_API_KEY": "sk-x"},
            "dir": "/data/worktrees/run-7",
            "read_only": ["/data/runs/7/agent"],
            "read_write": ["/data/worktrees/run-7", "/data/runs/7/tmp"],
            "urls": {"router": "http://127.0.0.1:7172", "gateway": "http://127.0.0.1:7172/mcp", "check": "http://127.0.0.1:7172"},
            "clone": "/data/repos/demo",
            "runner": runner,
        })
    }

    #[test]
    fn manifest_launch_leftovers_and_the_end_of_input() {
        let dir = tempfile::tempdir().unwrap();
        let nono = fake_nono(dir.path(), "0.81.0");
        let profiles = dir.path().join("profiles");
        let mut plugin = Plugin::start(&[
            "--nono",
            nono.to_str().unwrap(),
            "--profiles-dir",
            profiles.to_str().unwrap(),
            "--exclude-agent",
            "Moebius",
        ]);

        let manifest = plugin.line();
        assert_eq!(
            manifest,
            json!({"type": "manifest", "manifest": {"name": "nono-sandbox", "version": env!("CARGO_PKG_VERSION"), "kinds": ["launcher"]}})
        );

        // A wrapped launch.
        let reply = plugin.call(1, "launch", launch_params("Coder", Value::Null));
        assert_eq!(reply["type"], "reply", "{reply}");
        let result = &reply["result"];
        assert_eq!(result["answer"], "wrapped");
        assert_eq!(result["program"], nono.to_str().unwrap());
        let args: Vec<&str> = result["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap())
            .collect();
        assert_eq!(&args[..3], ["--silent", "run", "--profile"]);
        assert_eq!(&args[args.len() - 3..], ["pi-bolt", "--mode", "rpc"]);
        assert_eq!(result["env"]["MOEBIUS_RUN_TOKEN"], "moebius-run-x");
        assert!(result["env"].get("ANTHROPIC_API_KEY").is_none());
        assert!(!reply.to_string().contains("sk-x"));
        let profile = result["remove"][0].as_str().unwrap();
        assert!(std::path::Path::new(profile).is_file());
        assert!(result["stop"]["program"].is_string() && result["cleanup"]["args"][0] == "cleanup");

        // Its cleanup command removes the profile.
        let cleanup = &result["cleanup"];
        let status = Command::new(cleanup["program"].as_str().unwrap())
            .args(
                cleanup["args"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|a| a.as_str().unwrap()),
            )
            .status()
            .unwrap();
        assert!(status.success());
        assert!(!std::path::Path::new(profile).exists());

        // An excluded agent and a run on a runner are unchanged, with a note.
        for (id, request) in [
            (2, launch_params("Moebius", Value::Null)),
            (3, launch_params("Coder", json!("gpu-box"))),
        ] {
            let reply = plugin.call(id, "launch", request);
            assert_eq!(reply["result"]["answer"], "unchanged", "{reply}");
            assert!(reply["result"]["note"].is_string());
        }

        // Leftovers: a profile of a run that no longer exists is named.
        let old = profiles.join("run-9-abc.json");
        std::fs::write(&old, "{}").unwrap();
        let reply = plugin.call(4, "leftovers", json!({"running": []}));
        assert_eq!(
            reply["result"],
            json!({"cleanup": [], "remove": [old.to_str().unwrap()]})
        );
        let reply = plugin.call(5, "leftovers", json!({"running": [9]}));
        assert_eq!(reply["result"]["remove"], json!([]));

        // An error is an `error` line, not a crash.
        let reply = plugin.call(6, "decide", json!({}));
        assert_eq!(reply["type"], "error");

        // The plugin exits when its standard input closes.
        drop(plugin.input);
        assert!(plugin.child.wait().unwrap().success());
    }

    #[test]
    fn a_nono_of_another_version_stops_the_plugin_with_a_sentence() {
        let dir = tempfile::tempdir().unwrap();
        let nono = fake_nono(dir.path(), "0.99.0");
        let output = Command::new(env!("CARGO_BIN_EXE_nono-sandbox"))
            .args(["--nono", nono.to_str().unwrap()])
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty(), "no manifest is written");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("0.99.0") && stderr.contains("0.81.0"),
            "{stderr}"
        );
    }

    #[test]
    fn a_setting_that_is_wrong_stops_the_plugin_with_a_sentence() {
        let output = Command::new(env!("CARGO_BIN_EXE_nono-sandbox"))
            .args(["--network", "wide-open"])
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("wide-open"));
    }
}

#[cfg(windows)]
#[test]
fn the_plugin_refuses_to_start_on_windows() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_nono-sandbox"))
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Windows"));
}
