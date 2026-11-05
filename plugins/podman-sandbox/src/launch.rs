//! The plugin itself: the methods `launch` and `leftovers` of the kind `launcher`.
//!
//! `launch` answers `unchanged` (with a note) for a run on a runner and for an excluded agent, and otherwise answers
//! with this program's `run` subcommand around `podman run` (see [`crate::container`]), a stop command that removes
//! the container, and a note that names the image and the network. The wrapped program's environment is the
//! variables the container gets and the few Podman needs; the run token is among the first, named in the arguments
//! but never valued there. `leftovers` names the containers of runs that are not running, by their labels.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use moebius_plugin_api::{
    Call, Kind, LaunchAnswer, LaunchRequest, Leftovers, LeftoversRequest, Manifest, Plugin,
    PluginError, Wrapped,
};
use serde_json::Value;

use crate::NAME;
use crate::commands::{is_container_name, run_args, stop_command};
use crate::container::{NAME_PREFIX, PLUGIN_LABEL, RUN_LABEL, build, env_lines, passed_env};
use crate::settings::{Network, Settings};

/// The variables of the plugin's own environment that Podman reads to find its storage, its configuration and its
/// user session; the wrapped Podman and the stop command get them, so that they see what the checks at start saw.
pub const PODMAN_ENV: [&str; 9] = [
    "PATH",
    "HOME",
    "XDG_RUNTIME_DIR",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "DBUS_SESSION_BUS_ADDRESS",
    "CONTAINERS_CONF",
    "CONTAINERS_STORAGE_CONF",
    "CONTAINER_HOST",
];

/// The Podman sandbox launcher.
pub struct PodmanSandbox {
    settings: Settings,
    /// This program, which the wrapped launch and the stop command start.
    exe: PathBuf,
    /// Podman's own environment, from [`PODMAN_ENV`].
    podman_env: BTreeMap<String, String>,
}

impl PodmanSandbox {
    pub fn new(settings: Settings, exe: PathBuf, podman_env: BTreeMap<String, String>) -> Self {
        Self {
            settings,
            exe,
            podman_env,
        }
    }

    /// The variables of `env` that Podman needs.
    pub fn podman_env_of(
        env: impl IntoIterator<Item = (String, String)>,
    ) -> BTreeMap<String, String> {
        env.into_iter()
            .filter(|(name, _)| PODMAN_ENV.contains(&name.as_str()))
            .collect()
    }

    fn launch(&self, request: LaunchRequest) -> Result<LaunchAnswer, PluginError> {
        if request.runner.is_some() {
            return Ok(unchanged(
                "the run is on a runner, a machine this plugin's Podman does not reach, so it runs without the \
                 Podman sandbox",
            ));
        }
        if self.settings.exclude_agents.contains(&request.run.agent) {
            return Ok(unchanged(format!(
                "the agent {} is excluded from the Podman sandbox (exclude_agents), so it runs without it",
                request.run.agent
            )));
        }
        let failed = |e: crate::container::ContainerError| {
            PluginError::Failed(format!("cannot start a Podman container for the run: {e}"))
        };
        let mode = self.settings.network_for(&request.run.agent);
        let name = container_name(request.run.id);
        let (args, image) = build(&request, &self.settings, mode, &name).map_err(failed)?;

        let passed = passed_env(&request.env);
        // A variable that could not reach the container fails the launch here, with its name, not in the container.
        env_lines(&passed, passed.keys().map(String::as_str)).map_err(failed)?;
        let pass: Vec<String> = passed.keys().cloned().collect();
        let mut env = self.podman_env.clone();
        env.extend(passed);

        let reach = match mode {
            Network::Moebius => {
                "only the router's, the gateway's and the check's ports on the host's loopback"
            }
            Network::Internet => "the router's ports on the host's loopback and the internet",
        };
        Ok(LaunchAnswer::Wrapped(Box::new(Wrapped {
            program: self.exe.to_string_lossy().into_owned(),
            args: run_args(&self.settings.podman, &pass, args),
            env,
            urls: Default::default(),
            stop: Some(stop_command(
                &self.exe,
                &self.settings.podman,
                &name,
                &self.podman_env,
            )),
            cleanup: None,
            remove: Vec::new(),
            note: Some(format!(
                "in the Podman container {name} of the image {image}, which reaches {reach}"
            )),
        })))
    }

    /// Podman's list of this plugin's containers that cannot be read is an error: the containers could not be named,
    /// and the server should not start as if there were none.
    fn leftovers(&self, request: LeftoversRequest) -> Result<Leftovers, PluginError> {
        let running: BTreeSet<i64> = request.running.into_iter().collect();
        let output = std::process::Command::new(&self.settings.podman)
            .args([
                "ps",
                "--all",
                "--filter",
                &format!("label={PLUGIN_LABEL}={NAME}"),
                "--format",
                "json",
            ])
            .env_clear()
            .envs(&self.podman_env)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| {
                PluginError::Failed(format!("cannot run {}: {e}", self.settings.podman))
            })?;
        let containers: Value = output
            .status
            .success()
            .then(|| serde_json::from_slice(&output.stdout).ok())
            .flatten()
            .ok_or_else(|| {
                PluginError::Failed(format!(
                    "cannot list this plugin's containers: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ))
            })?;
        let mut names: Vec<String> = containers
            .as_array()
            .into_iter()
            .flatten()
            .filter(|container| {
                let run = container["Labels"][RUN_LABEL]
                    .as_str()
                    .and_then(|id| id.parse::<i64>().ok());
                run.is_none_or(|id| !running.contains(&id))
            })
            .flat_map(|container| container["Names"].as_array().cloned().unwrap_or_default())
            .filter_map(|name| name.as_str().map(String::from))
            .filter(|name| is_container_name(name))
            .collect();
        names.sort();
        Ok(Leftovers {
            cleanup: names
                .iter()
                .map(|name| stop_command(&self.exe, &self.settings.podman, name, &self.podman_env))
                .collect(),
            remove: Vec::new(),
        })
    }
}

fn unchanged(note: impl Into<String>) -> LaunchAnswer {
    LaunchAnswer::Unchanged {
        note: Some(note.into()),
    }
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A new name for a container of the run `run`: `moebius-run-<id>-<nonce>`, since a chat's Pi is launched again for
/// each message and the last container may not be gone yet.
fn container_name(run: i64) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!(
        "{NAME_PREFIX}{run}-{:x}{:x}",
        nanos,
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

impl Plugin for PodmanSandbox {
    fn manifest(&self) -> Manifest {
        Manifest {
            name: NAME.into(),
            version: env!("CARGO_PKG_VERSION").into(),
            kinds: vec![Kind::Launcher],
        }
    }

    fn call(&self, call: Call) -> Result<Value, PluginError> {
        let bad = |e: serde_json::Error| PluginError::BadParams(e.to_string());
        let failed = |e: serde_json::Error| PluginError::Failed(e.to_string());
        match call.method.as_str() {
            "launch" => {
                let request = serde_json::from_value(call.params).map_err(bad)?;
                serde_json::to_value(self.launch(request)?).map_err(failed)
            }
            "leftovers" => {
                let request = serde_json::from_value(call.params).map_err(bad)?;
                serde_json::to_value(self.leftovers(request)?).map_err(failed)
            }
            other => Err(PluginError::Failed(format!(
                "the Podman sandbox has no method {other:?}"
            ))),
        }
    }
}

// The paths in these tests are Unix paths.
#[cfg(all(test, unix))]
mod tests {
    use std::path::Path;

    use moebius_plugin_api::{LaunchRun, Urls};

    use super::*;

    struct World {
        _dir: tempfile::TempDir,
        root: PathBuf,
    }

    fn world() -> World {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        for sub in ["bin", "data/worktrees/run-7", "data/runs/7/tmp"] {
            std::fs::create_dir_all(root.join(sub)).unwrap();
        }
        std::fs::write(root.join("bin/pi-bolt"), "").unwrap();
        World { _dir: dir, root }
    }

    fn request(world: &World, agent: &str) -> LaunchRequest {
        let at = |p: &str| world.root.join(p).to_string_lossy().into_owned();
        LaunchRequest {
            run: LaunchRun {
                id: 7,
                purpose: "code".into(),
                agent: agent.into(),
                chat: false,
                card: Some(3),
            },
            program: "pi-bolt".into(),
            args: vec!["--mode".into(), "rpc".into()],
            env: [
                ("PATH", at("bin")),
                ("MOEBIUS_RUN_TOKEN", "moebius-run-secret-token".into()),
                ("OPENAI_API_KEY", "sk-provider".into()),
            ]
            .map(|(k, v)| (k.to_string(), v))
            .into(),
            dir: at("data/worktrees/run-7"),
            read_only: vec![],
            read_write: vec![at("data/worktrees/run-7"), at("data/runs/7/tmp")],
            urls: Urls {
                router: "http://127.0.0.1:7172".into(),
                gateway: "http://127.0.0.1:7172/mcp".into(),
                check: "http://127.0.0.1:7172".into(),
            },
            clone: None,
            runner: None,
        }
    }

    fn plugin(edit: impl FnOnce(&mut Settings)) -> PodmanSandbox {
        let mut settings = Settings {
            podman: "/usr/bin/podman".into(),
            image: Some("localhost/base".into()),
            ..Settings::default()
        };
        edit(&mut settings);
        let env = PodmanSandbox::podman_env_of([
            ("PATH".to_string(), "/usr/bin".to_string()),
            ("XDG_RUNTIME_DIR".to_string(), "/run/user/1000".to_string()),
            ("SECRET_OF_THE_SERVER".to_string(), "x".to_string()),
        ]);
        PodmanSandbox::new(settings, PathBuf::from("/opt/plugins/podman-sandbox"), env)
    }

    fn wrapped(answer: LaunchAnswer) -> Wrapped {
        match answer {
            LaunchAnswer::Wrapped(wrapped) => *wrapped,
            other => panic!("not wrapped: {other:?}"),
        }
    }

    #[test]
    fn a_launch_is_this_program_around_podman_run() {
        let world = world();
        let wrapped = wrapped(plugin(|_| {}).launch(request(&world, "Coder")).unwrap());
        assert_eq!(wrapped.program, "/opt/plugins/podman-sandbox");
        assert_eq!(
            &wrapped.args[..7],
            [
                "run",
                "--podman",
                "/usr/bin/podman",
                "--pass",
                "MOEBIUS_RUN_TOKEN",
                "--",
                "run"
            ]
        );
        assert_eq!(&wrapped.args[wrapped.args.len() - 2..], ["--mode", "rpc"]);
        assert!(wrapped.urls.router.is_none() && wrapped.urls.gateway.is_none());
        let note = wrapped.note.unwrap();
        assert!(
            note.contains("localhost/base") && note.contains("only the router"),
            "{note}"
        );
    }

    #[test]
    fn the_run_token_is_in_the_environment_and_never_in_argv() {
        let world = world();
        let wrapped = wrapped(plugin(|_| {}).launch(request(&world, "Coder")).unwrap());
        assert_eq!(wrapped.env["MOEBIUS_RUN_TOKEN"], "moebius-run-secret-token");
        assert_eq!(wrapped.env["XDG_RUNTIME_DIR"], "/run/user/1000");
        assert!(!wrapped.env.contains_key("OPENAI_API_KEY"));
        assert!(!wrapped.env.contains_key("SECRET_OF_THE_SERVER"));
        let everything = serde_json::to_string(&(&wrapped.args, &wrapped.stop)).unwrap();
        assert!(
            !everything.contains("secret-token") && !everything.contains("sk-provider"),
            "{everything}"
        );
    }

    #[test]
    fn the_stop_command_removes_this_container_with_podmans_environment() {
        let world = world();
        let plugin = plugin(|_| {});
        let wrapped = wrapped(plugin.launch(request(&world, "Coder")).unwrap());
        let stop = wrapped.stop.unwrap();
        assert_eq!(stop.program, "/opt/plugins/podman-sandbox");
        assert_eq!(&stop.args[..2], ["stop", "/usr/bin/podman"]);
        let at = wrapped.args.iter().position(|a| a == "--name").unwrap();
        assert_eq!(wrapped.args[at + 1], stop.args[2]);
        assert!(is_container_name(&stop.args[2]));
        assert_eq!(stop.env["XDG_RUNTIME_DIR"], "/run/user/1000");
        assert!(!stop.env.contains_key("MOEBIUS_RUN_TOKEN"));
        assert!(wrapped.cleanup.is_none() && wrapped.remove.is_empty());

        let again = self::wrapped(plugin.launch(request(&world, "Coder")).unwrap());
        assert_ne!(
            again.stop.unwrap().args[2],
            stop.args[2],
            "two launches, two containers"
        );
    }

    #[test]
    fn the_network_mode_is_per_agent() {
        let world = world();
        let plugin = plugin(|s| {
            s.network = Network::Internet;
            s.agent_network.insert("Coder".into(), Network::Moebius);
        });
        let network = |agent: &str| {
            let args = self::wrapped(plugin.launch(request(&world, agent)).unwrap()).args;
            let at = args.iter().position(|a| a == "--network").unwrap();
            args[at + 1].clone()
        };
        assert_eq!(network("Coder"), "pasta:--splice-only,-T,7172");
        assert_eq!(network("Researcher"), "pasta:-T,7172");
    }

    #[test]
    fn an_excluded_agent_and_a_run_on_a_runner_are_unchanged_with_a_note() {
        let world = world();
        let plugin = plugin(|s| {
            s.exclude_agents.insert("Moebius".into());
        });
        let LaunchAnswer::Unchanged { note: Some(note) } =
            plugin.launch(request(&world, "Moebius")).unwrap()
        else {
            panic!("not unchanged")
        };
        assert!(
            note.contains("Moebius") && note.contains("exclude_agents"),
            "{note}"
        );
        let mut remote = request(&world, "Coder");
        remote.runner = Some("gpu-box".into());
        let LaunchAnswer::Unchanged { note: Some(note) } = plugin.launch(remote).unwrap() else {
            panic!("not unchanged")
        };
        assert!(note.contains("runner"), "{note}");
    }

    #[test]
    fn a_run_without_an_image_or_with_a_broken_variable_fails_the_launch() {
        let world = world();
        let error = plugin(|s| s.image = None)
            .launch(request(&world, "Coder"))
            .unwrap_err();
        assert!(error.to_string().contains("no image"), "{error}");
        let mut broken = request(&world, "Coder");
        broken.env.insert("LANG".into(), "C\nX=1".into());
        let error = plugin(|_| {}).launch(broken).unwrap_err();
        assert!(error.to_string().contains("LANG"), "{error}");
    }

    /// A fake Podman whose `ps` prints `json`.
    fn with_fake_ps(dir: &Path, json: &str, code: i32) -> PodmanSandbox {
        let podman = dir.join("podman");
        crate::testing::script(
            &podman,
            &format!(
                "#!/bin/sh\ncat <<'EOF'\n{json}\nEOF\necho 'said on stderr' >&2\nexit {code}\n"
            ),
        );
        plugin(|s| s.podman = podman.to_string_lossy().into_owned())
    }

    #[test]
    fn leftovers_are_the_containers_of_runs_that_are_not_running() {
        let dir = tempfile::tempdir().unwrap();
        let json = serde_json::json!([
            {"Names": ["moebius-run-3-aa"], "Labels": {"moebius.plugin": "podman-sandbox", "moebius.run": "3"}},
            {"Names": ["moebius-run-4-bb"], "Labels": {"moebius.plugin": "podman-sandbox", "moebius.run": "4"}},
            {"Names": ["someone-else"], "Labels": {"moebius.plugin": "podman-sandbox", "moebius.run": "5"}},
        ]);
        let plugin = with_fake_ps(dir.path(), &json.to_string(), 0);
        let left = plugin
            .leftovers(LeftoversRequest { running: vec![4] })
            .unwrap();
        let names: Vec<&str> = left.cleanup.iter().map(|c| c.args[2].as_str()).collect();
        assert_eq!(names, ["moebius-run-3-aa"]);
        assert_eq!(left.cleanup[0].args[0], "stop");
        assert!(left.remove.is_empty());
        let all = plugin.leftovers(LeftoversRequest::default()).unwrap();
        assert_eq!(all.cleanup.len(), 2);
    }

    #[test]
    fn a_podman_that_cannot_list_its_containers_is_an_error_not_an_empty_answer() {
        let dir = tempfile::tempdir().unwrap();
        let error = with_fake_ps(dir.path(), "", 125)
            .leftovers(LeftoversRequest::default())
            .unwrap_err();
        assert!(error.to_string().contains("said on stderr"), "{error}");
        let error = with_fake_ps(dir.path(), "not json", 0)
            .leftovers(LeftoversRequest::default())
            .unwrap_err();
        assert!(error.to_string().contains("cannot list"), "{error}");
        let none = with_fake_ps(dir.path(), "[]", 0)
            .leftovers(LeftoversRequest::default())
            .unwrap();
        assert!(none.cleanup.is_empty());
    }

    #[test]
    fn the_manifest_is_a_launcher_and_other_methods_are_refused() {
        let plugin = plugin(|_| {});
        let manifest = plugin.manifest();
        assert_eq!(
            (manifest.name.as_str(), manifest.kinds),
            (NAME, vec![Kind::Launcher])
        );
        let error = plugin
            .call(Call::new(Kind::Launcher, "decide", Value::Null))
            .unwrap_err();
        assert!(error.to_string().contains("no method"));
        let error = plugin
            .call(Call::new(
                Kind::Launcher,
                "launch",
                serde_json::json!({"x": 1}),
            ))
            .unwrap_err();
        assert!(matches!(error, PluginError::BadParams(_)));
    }
}
