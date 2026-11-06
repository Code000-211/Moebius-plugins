//! The plugin itself: the methods `launch` and `leftovers` of the kind `launcher`.
//!
//! `launch` answers `unchanged` (with a note) for a run on a runner and for an excluded agent, and otherwise answers
//! with this program's `run` subcommand around the engine's `run` (see [`crate::container`]), a stop command that
//! removes the container, and a note that names the engine, the image and the network. The wrapped program's
//! environment is the variables the container gets and the few the engine needs; the run token is among the first,
//! named in the arguments but never valued there. A Docker run also gets a directory for its relay's sockets, which
//! the core removes after it. `leftovers` names the containers of runs that are not running, by their labels.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use moebius_plugin_api::{
    Call, Kind, LaunchAnswer, LaunchRequest, Leftovers, LeftoversRequest, Manifest, Plugin,
    PluginError, PluginSetting, SettingInput, Wrapped,
};
use serde_json::Value;

use crate::NAME;
use crate::commands::{Build, Own, Relay, Wrap, is_container_name, stop_command};
use crate::container::{
    BASE_ARG, BASE_DOCKERFILE, Container, DOCKERFILE, Host, NAME_PREFIX, PLUGIN_LABEL, RUN_LABEL,
    User, build, env_lines, passed_env,
};
use crate::mounts::MOUNTS;
use crate::settings::{Engine, Network, Settings};

/// The variables of the plugin's own environment that the engine reads to find its storage or its daemon, its
/// configuration and its user session; the wrapped engine and the stop command get them, so that they see what the
/// checks at start saw.
pub const ENGINE_ENV: [&str; 14] = [
    "PATH",
    "HOME",
    "XDG_RUNTIME_DIR",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "DBUS_SESSION_BUS_ADDRESS",
    "CONTAINERS_CONF",
    "CONTAINERS_STORAGE_CONF",
    "CONTAINER_HOST",
    "DOCKER_HOST",
    "DOCKER_CONTEXT",
    "DOCKER_CONFIG",
    "DOCKER_CERT_PATH",
    "DOCKER_TLS_VERIFY",
];

/// The container sandbox launcher.
pub struct ContainerSandbox {
    settings: Settings,
    /// This program, which the wrapped launch and the stop command start, and which relays in a Docker container.
    exe: PathBuf,
    /// The engine's own environment, from [`ENGINE_ENV`].
    engine_env: BTreeMap<String, String>,
    /// The user of the server, whom a Docker container runs as.
    user: User,
}

impl ContainerSandbox {
    pub fn new(
        settings: Settings,
        exe: PathBuf,
        engine_env: BTreeMap<String, String>,
        user: User,
    ) -> Self {
        Self {
            settings,
            exe,
            engine_env,
            user,
        }
    }

    /// The variables of `env` that the engine needs.
    pub fn engine_env_of(
        env: impl IntoIterator<Item = (String, String)>,
    ) -> BTreeMap<String, String> {
        env.into_iter()
            .filter(|(name, _)| ENGINE_ENV.contains(&name.as_str()))
            .collect()
    }

    fn launch(&self, request: LaunchRequest) -> Result<LaunchAnswer, PluginError> {
        let engine = self.settings.engine;
        if request.runner.is_some() {
            return Ok(unchanged(format!(
                "the run is on a runner, a machine this plugin's {} does not reach, so it runs without the \
                 container sandbox",
                engine.title()
            )));
        }
        if self.settings.exclude_agents.contains(&request.run.agent) {
            return Ok(unchanged(format!(
                "the agent {} is excluded from the container sandbox (exclude_agents), so it runs without it",
                request.run.agent
            )));
        }
        let failed = |e: crate::container::ContainerError| {
            PluginError::Failed(format!(
                "cannot start a {} container for the run: {e}",
                engine.title()
            ))
        };
        let mode = self.settings.network_for(&request.run.agent);
        let name = container_name(request.run.id);
        let host = Host {
            user: self.user,
            program: self.exe.clone(),
            relay: std::env::temp_dir().join(format!("{name}-relay")),
        };
        let Container {
            args,
            image,
            volumes,
            ports,
        } = build(&request, &self.settings, mode, &name, &host).map_err(failed)?;
        // Dockerfiles are built by the wrapped launch, which has no time limit, each from a directory that holds it
        // alone; the core removes the directories once the run has ended.
        let builds = image
            .builds
            .iter()
            .enumerate()
            .map(|(at, dockerfile)| {
                Ok(Build {
                    image: dockerfile.image.clone(),
                    dir: build_context(&format!("{name}-{at}"), &dockerfile.text)?,
                    base: dockerfile.base.clone(),
                })
            })
            .collect::<Result<Vec<_>, PluginError>>()?;
        let mut remove: Vec<PathBuf> = builds.iter().map(|build| build.dir.clone()).collect();
        let (own, relay) = match engine {
            Engine::Podman => (None, None),
            Engine::Docker => {
                relay_dir(&host.relay)?;
                remove.push(host.relay.clone());
                let own = (!volumes.is_empty()).then(|| Own {
                    image: image.name.clone(),
                    user: self.user,
                    volumes,
                });
                (
                    own,
                    Some(Relay {
                        dir: host.relay.clone(),
                        ports,
                    }),
                )
            }
        };

        let passed = passed_env(&request.env);
        // A variable that could not reach the container fails the launch here, with its name, not in the container.
        env_lines(&passed, passed.keys().map(String::as_str)).map_err(failed)?;
        let pass: Vec<String> = passed.keys().cloned().collect();
        let mut env = self.engine_env.clone();
        env.extend(passed);

        let reach = match mode {
            Network::Moebius => {
                "only the router's, the gateway's and the check's ports on the host's loopback"
            }
            Network::Internet => "the router's ports on the host's loopback and the internet",
        };
        let wrap = Wrap {
            engine,
            program: self.settings.program.clone(),
            pass,
            builds,
            own,
            relay,
            args,
        };
        Ok(LaunchAnswer::Wrapped(Box::new(Wrapped {
            program: self.exe.to_string_lossy().into_owned(),
            args: wrap.to_args(),
            env,
            urls: Default::default(),
            stop: Some(stop_command(
                &self.exe,
                engine,
                &self.settings.program,
                &name,
                &self.engine_env,
            )),
            cleanup: None,
            remove: remove
                .iter()
                .map(|dir| dir.to_string_lossy().into_owned())
                .collect(),
            note: Some(format!(
                "in the {} container {name} of the image {}{}, which reaches {reach}{}",
                engine.title(),
                image.name,
                match image.builds.as_slice() {
                    [] => "",
                    [_] => " (built from a Dockerfile)",
                    _ => " (built from a Dockerfile on the server's base Dockerfile)",
                },
                match engine {
                    Engine::Podman => "",
                    Engine::Docker => " (the ports relayed by this plugin)",
                },
            )),
        })))
    }

    /// The engine's list of this plugin's containers that cannot be read is an error: the containers could not be
    /// named, and the server should not start as if there were none.
    fn leftovers(&self, request: LeftoversRequest) -> Result<Leftovers, PluginError> {
        let running: BTreeSet<i64> = request.running.into_iter().collect();
        let program = &self.settings.program;
        // Podman lists JSON; Docker's JSON has neither its names nor its labels as lists, so it is asked for lines.
        let format = match self.settings.engine {
            Engine::Podman => "json".to_string(),
            Engine::Docker => format!("{{{{.Names}}}}\t{{{{.Label \"{RUN_LABEL}\"}}}}"),
        };
        let output = std::process::Command::new(program)
            .args([
                "ps",
                "--all",
                "--filter",
                &format!("label={PLUGIN_LABEL}={NAME}"),
                "--format",
                &format,
            ])
            .env_clear()
            .envs(&self.engine_env)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| PluginError::Failed(format!("cannot run {program}: {e}")))?;
        let cannot = || {
            PluginError::Failed(format!(
                "cannot list this plugin's containers: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ))
        };
        if !output.status.success() {
            return Err(cannot());
        }
        let containers: Vec<(Vec<String>, Option<i64>)> = match self.settings.engine {
            Engine::Podman => {
                let listed: Value = serde_json::from_slice(&output.stdout).map_err(|_| cannot())?;
                listed
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|container| {
                        let names = container["Names"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|name| name.as_str().map(String::from))
                            .collect();
                        let run = container["Labels"][RUN_LABEL]
                            .as_str()
                            .and_then(|id| id.parse().ok());
                        (names, run)
                    })
                    .collect()
            }
            Engine::Docker => String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(|line| {
                    let (names, run) = line.split_once('\t').unwrap_or((line, ""));
                    let names = names.split(',').map(String::from).collect();
                    (names, run.trim().parse().ok())
                })
                .collect(),
        };
        let mut names: Vec<String> = containers
            .into_iter()
            .filter(|(_, run)| run.is_none_or(|id| !running.contains(&id)))
            .flat_map(|(names, _)| names)
            .filter(|name| is_container_name(name))
            .collect();
        names.sort();
        Ok(Leftovers {
            cleanup: names
                .iter()
                .map(|name| {
                    stop_command(
                        &self.exe,
                        self.settings.engine,
                        program,
                        name,
                        &self.engine_env,
                    )
                })
                .collect(),
            remove: Vec::new(),
        })
    }
}

/// Writes `dockerfile` alone into a new directory for the container `name`, the build context of its image: nothing of
/// the host is in it, so a `COPY` finds nothing to copy.
fn build_context(name: &str, dockerfile: &str) -> Result<PathBuf, PluginError> {
    let dir = std::env::temp_dir().join(format!("{name}-build"));
    std::fs::create_dir(&dir)
        .and_then(|()| std::fs::write(dir.join("Dockerfile"), dockerfile))
        .map_err(|e| {
            PluginError::Failed(format!(
                "cannot write a Dockerfile to {}: {e}",
                dir.display()
            ))
        })?;
    Ok(dir)
}

/// Makes the directory of a Docker run's relay sockets, which only the server's user may enter: a socket in it is a
/// way to the router.
fn relay_dir(dir: &std::path::Path) -> Result<(), PluginError> {
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir).map_err(|e| {
        PluginError::Failed(format!(
            "cannot make the directory of the relay's sockets {}: {e}",
            dir.display()
        ))
    })
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

impl Plugin for ContainerSandbox {
    fn manifest(&self) -> Manifest {
        Manifest {
            name: NAME.into(),
            version: env!("CARGO_PKG_VERSION").into(),
            kinds: vec![Kind::Launcher],
            project_settings: vec![PluginSetting {
                key: DOCKERFILE.into(),
                label: "Dockerfile".into(),
                description: format!(
                    "The image of this repository's runs: the plugin builds it with its engine (Podman or \
                     Docker) before the first run that needs it, from an empty build context, and builds it again when it changes. When the \
                     server has a base Dockerfile (on the Settings page), start from it with `ARG {BASE_ARG}` and \
                     `FROM ${{{BASE_ARG}}}`. Left empty, the runs use --repo-image, else the base Dockerfile, \
                     else --image."
                ),
                input: SettingInput::Code,
                language: Some("dockerfile".into()),
            }, PluginSetting {
                key: MOUNTS.into(),
                label: "Mounts".into(),
                description: "Folders of the host and named volumes of the engine this repository's runs see, one per \
                     line: <host path>:<container path>[:ro|:rw], read only unless rw, or \
                     volume:<name>:<container path>[:ro|:rw], read and write unless ro and kept between runs. A \
                     mount that would reach Moebius's data directory, a hidden directory of the home, or a path the \
                     run already has fails the run."
                    .into(),
                input: SettingInput::Code,
                language: None,
            }],
            app_settings: vec![PluginSetting {
                key: BASE_DOCKERFILE.into(),
                label: "Base Dockerfile".into(),
                description: format!(
                    "The image every repository's Dockerfile starts from: what the harness needs in a container \
                     (a glibc-based system with /bin/sh, node if the harness is a node package, git) and the \
                     tools all projects share. Built like a repository's Dockerfile and again when it changes, \
                     which builds the repositories' images again. A repository's Dockerfile begins with \
                     `ARG {BASE_ARG}` and `FROM ${{{BASE_ARG}}}`; a repository without one runs on this image."
                ),
                input: SettingInput::Code,
                language: Some("dockerfile".into()),
            }],
            settings_ui: Some(crate::page::source().into()),
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
                "the container sandbox has no method {other:?}"
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
            repository: None,
            project_settings: BTreeMap::new(),
            app_settings: BTreeMap::new(),
        }
    }

    fn plugin(edit: impl FnOnce(&mut Settings)) -> ContainerSandbox {
        let mut settings = Settings {
            program: "/usr/bin/podman".into(),
            image: Some("localhost/base".into()),
            ..Settings::default()
        };
        edit(&mut settings);
        let env = ContainerSandbox::engine_env_of([
            ("PATH".to_string(), "/usr/bin".to_string()),
            ("XDG_RUNTIME_DIR".to_string(), "/run/user/1000".to_string()),
            ("SECRET_OF_THE_SERVER".to_string(), "x".to_string()),
        ]);
        ContainerSandbox::new(
            settings,
            PathBuf::from("/opt/plugins/container-sandbox"),
            env,
            User {
                uid: 1000,
                gid: 1000,
            },
        )
    }

    fn wrapped(answer: LaunchAnswer) -> Wrapped {
        match answer {
            LaunchAnswer::Wrapped(wrapped) => *wrapped,
            other => panic!("not wrapped: {other:?}"),
        }
    }

    #[test]
    fn a_launch_is_this_program_around_the_engines_run() {
        let world = world();
        let wrapped = wrapped(
            plugin(|s| s.network = Network::Moebius)
                .launch(request(&world, "Coder"))
                .unwrap(),
        );
        assert_eq!(wrapped.program, "/opt/plugins/container-sandbox");
        assert_eq!(
            &wrapped.args[..9],
            [
                "run",
                "--engine",
                "podman",
                "/usr/bin/podman",
                "--pass",
                "MOEBIUS_RUN_TOKEN",
                "--",
                "run",
                "--rm"
            ]
        );
        assert_eq!(&wrapped.args[wrapped.args.len() - 2..], ["--mode", "rpc"]);
        assert!(wrapped.urls.router.is_none() && wrapped.urls.gateway.is_none());
        let note = wrapped.note.unwrap();
        assert!(
            note.contains("Podman container")
                && note.contains("localhost/base")
                && note.contains("only the router"),
            "{note}"
        );
    }

    #[test]
    fn a_docker_launch_relays_the_ports_through_a_directory_the_core_removes() {
        let world = world();
        let plugin = plugin(|s| {
            s.engine = Engine::Docker;
            s.program = "/usr/bin/docker".into();
            s.caches.push(crate::settings::Cache {
                volume: "moebius-npm".into(),
                path: "/home/moebius/.npm".into(),
            });
        });
        let wrapped = wrapped(plugin.launch(request(&world, "Coder")).unwrap());
        assert_eq!(
            &wrapped.args[..4],
            ["run", "--engine", "docker", "/usr/bin/docker"]
        );
        let at = wrapped.args.iter().position(|a| a == "--relay").unwrap();
        let dir = PathBuf::from(&wrapped.args[at + 1]);
        assert_eq!(wrapped.args[at + 2], "7172");
        assert_eq!(wrapped.remove, [dir.to_string_lossy().into_owned()]);
        assert!(dir.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
            assert_eq!(
                mode & 0o777,
                0o700,
                "only the user may reach the router through it"
            );
        }
        // The sockets' directory is the one the container mounts.
        let inside = format!("{}:{}:ro,z", dir.display(), crate::container::RELAY_DIR);
        assert!(wrapped.args.contains(&inside), "{:?}", wrapped.args);
        // The cache is given to the user in the image of the run, before it.
        let at = wrapped.args.iter().position(|a| a == "--own").unwrap();
        assert_eq!(
            wrapped.args[at + 1..at + 4],
            ["localhost/base", "1000:1000", "moebius-npm"]
        );
        let stop = wrapped.stop.unwrap();
        assert_eq!(&stop.args[..3], ["stop", "docker", "/usr/bin/docker"]);
        let note = wrapped.note.unwrap();
        assert!(
            note.contains("Docker container") && note.contains("relayed"),
            "{note}"
        );
        std::fs::remove_dir(dir).unwrap();

        // Without volumes, nothing is owned.
        let plugin = self::plugin(|s| s.engine = Engine::Docker);
        let wrapped = self::wrapped(plugin.launch(request(&world, "Coder")).unwrap());
        assert!(!wrapped.args.contains(&"--own".to_string()));
        for dir in wrapped.remove {
            std::fs::remove_dir(dir).unwrap();
        }
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
    fn the_stop_command_removes_this_container_with_the_engines_environment() {
        let world = world();
        let plugin = plugin(|_| {});
        let wrapped = wrapped(plugin.launch(request(&world, "Coder")).unwrap());
        let stop = wrapped.stop.unwrap();
        assert_eq!(stop.program, "/opt/plugins/container-sandbox");
        assert_eq!(&stop.args[..3], ["stop", "podman", "/usr/bin/podman"]);
        let at = wrapped.args.iter().position(|a| a == "--name").unwrap();
        assert_eq!(wrapped.args[at + 1], stop.args[3]);
        assert!(is_container_name(&stop.args[3]));
        assert_eq!(stop.env["XDG_RUNTIME_DIR"], "/run/user/1000");
        assert!(!stop.env.contains_key("MOEBIUS_RUN_TOKEN"));
        assert!(wrapped.cleanup.is_none() && wrapped.remove.is_empty());

        let again = self::wrapped(plugin.launch(request(&world, "Coder")).unwrap());
        assert_ne!(
            again.stop.unwrap().args[3],
            stop.args[3],
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
    fn with_fake_ps(dir: &Path, json: &str, code: i32) -> ContainerSandbox {
        with_fake_engine_ps(dir, Engine::Podman, json, code)
    }

    /// A fake engine whose `ps` prints `listed` and writes its arguments to `ps.args`.
    fn with_fake_engine_ps(
        dir: &Path,
        engine: Engine,
        listed: &str,
        code: i32,
    ) -> ContainerSandbox {
        let program = dir.join(engine.name());
        crate::testing::script(
            &program,
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\ncat <<'EOF'\n{listed}\nEOF\necho 'said on stderr' >&2\nexit {code}\n",
                dir.join("ps.args").display()
            ),
        );
        plugin(|s| {
            s.engine = engine;
            s.program = program.to_string_lossy().into_owned();
        })
    }

    #[test]
    fn docker_leftovers_are_read_from_its_lines() {
        let dir = tempfile::tempdir().unwrap();
        let listed =
            "moebius-run-3-aa\t3\nmoebius-run-4-bb\t4\nsomeone-else\t5\nmoebius-run-9-cc\t\n";
        let plugin = with_fake_engine_ps(dir.path(), Engine::Docker, listed, 0);
        let left = plugin
            .leftovers(LeftoversRequest { running: vec![4] })
            .unwrap();
        let names: Vec<&str> = left.cleanup.iter().map(|c| c.args[3].as_str()).collect();
        assert_eq!(names, ["moebius-run-3-aa", "moebius-run-9-cc"]);
        assert_eq!(&left.cleanup[0].args[..2], ["stop", "docker"]);
        let asked = std::fs::read_to_string(dir.path().join("ps.args")).unwrap();
        assert!(
            asked.contains("label=moebius.plugin=container-sandbox")
                && asked.contains("{{.Names}}\t{{.Label \"moebius.run\"}}"),
            "{asked}"
        );
        let error = with_fake_engine_ps(dir.path(), Engine::Docker, "", 1)
            .leftovers(LeftoversRequest::default())
            .unwrap_err();
        assert!(error.to_string().contains("said on stderr"), "{error}");
    }

    #[test]
    fn leftovers_are_the_containers_of_runs_that_are_not_running() {
        let dir = tempfile::tempdir().unwrap();
        let json = serde_json::json!([
            {"Names": ["moebius-run-3-aa"], "Labels": {"moebius.plugin": "container-sandbox", "moebius.run": "3"}},
            {"Names": ["moebius-run-4-bb"], "Labels": {"moebius.plugin": "container-sandbox", "moebius.run": "4"}},
            {"Names": ["someone-else"], "Labels": {"moebius.plugin": "container-sandbox", "moebius.run": "5"}},
        ]);
        let plugin = with_fake_ps(dir.path(), &json.to_string(), 0);
        let left = plugin
            .leftovers(LeftoversRequest { running: vec![4] })
            .unwrap();
        let names: Vec<&str> = left.cleanup.iter().map(|c| c.args[3].as_str()).collect();
        assert_eq!(names, ["moebius-run-3-aa"]);
        assert_eq!(left.cleanup[0].args[0], "stop");
        assert!(left.remove.is_empty());
        let all = plugin.leftovers(LeftoversRequest::default()).unwrap();
        assert_eq!(all.cleanup.len(), 2);
    }

    #[test]
    fn an_engine_that_cannot_list_its_containers_is_an_error_not_an_empty_answer() {
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
    fn the_manifest_is_a_launcher_with_its_page_and_other_methods_are_refused() {
        let plugin = plugin(|_| {});
        let manifest = plugin.manifest();
        assert_eq!(
            manifest.settings_ui.as_deref(),
            Some(crate::page::source().as_str())
        );
        let keys: Vec<&str> = manifest
            .project_settings
            .iter()
            .map(|setting| setting.key.as_str())
            .collect();
        assert_eq!(keys, [DOCKERFILE, MOUNTS]);
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
