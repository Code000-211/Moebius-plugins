//! The plugin itself: the methods `launch` and `leftovers` of the kind `launcher`.
//!
//! `launch` answers `unchanged` (with a note) for a run on a runner and for an excluded agent, and otherwise writes
//! the run's profile into the profiles directory, owner-only, and answers with the wrapped launch
//! `nono run --profile <file> --name <session> -- <program> <args>`. Everything the run must not see is left out of the
//! profile and out of the environment; the environment is the process environment of `nono`, not its arguments.
//! `leftovers` names the profiles of runs that are not running.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use moebius_plugin_api::{
    Call, Kind, LaunchAnswer, LaunchRequest, Leftovers, LeftoversRequest, Manifest, Plugin,
    PluginError, Wrapped,
};
use serde_json::Value;

use crate::commands::{SESSION_PREFIX, cleanup_command, stop_command};
use crate::profile::{build, kept_env, network_note, socket_note};
use crate::settings::Settings;
use crate::{NAME, PINNED_NONO_VERSION};

/// The nono sandbox launcher.
pub struct NonoSandbox {
    settings: Settings,
    /// This program, which the stop and cleanup commands start.
    exe: PathBuf,
}

impl NonoSandbox {
    pub fn new(settings: Settings, exe: PathBuf) -> Self {
        Self { settings, exe }
    }

    fn launch(&self, request: LaunchRequest) -> Result<LaunchAnswer, PluginError> {
        if request.runner.is_some() {
            return Ok(unchanged(
                "the run is on a runner, a machine this sandbox cannot reach, so it runs without the nono sandbox",
            ));
        }
        if self.settings.exclude_agents.contains(&request.run.agent) {
            return Ok(unchanged(format!(
                "the agent {} is excluded from the nono sandbox (exclude_agents), so it runs without it",
                request.run.agent
            )));
        }
        let mode = self.settings.network_for(&request.run.agent);
        let os = std::env::consts::OS;
        let notes: Vec<String> = [
            network_note(&request, mode, os),
            socket_note(&self.settings, os),
        ]
        .into_iter()
        .flatten()
        .collect();
        let note = (!notes.is_empty()).then(|| notes.join("; "));
        let profile = build(&request, &self.settings, mode, os)
            .map_err(|e| PluginError::Failed(format!("cannot write a nono profile: {e}")))?;
        let file = write_profile(&self.settings.profiles_dir, request.run.id, &profile)
            .map_err(|e| PluginError::Failed(format!("cannot write a nono profile: {e}")))?;
        let stem = file
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let session = format!("{SESSION_PREFIX}{stem}");

        let mut args = vec![
            "--silent".to_string(),
            "run".into(),
            "--profile".into(),
            file.to_string_lossy().into_owned(),
            "--name".into(),
            session.clone(),
            // Moebius keeps its own record of runs; nono's audit and session records would pile up, each with the
            // whole command line, in the user's state directory.
            "--no-audit".into(),
            "--no-rollback".into(),
            "--".into(),
            request.program,
        ];
        args.extend(request.args);
        Ok(LaunchAnswer::Wrapped(Box::new(Wrapped {
            program: self.settings.nono.clone(),
            args,
            env: kept_env(&request.env),
            urls: Default::default(),
            stop: Some(stop_command(&self.exe, &self.settings.nono, &session)),
            cleanup: Some(cleanup_command(&self.exe, &file)),
            remove: vec![file.to_string_lossy().into_owned()],
            note,
        })))
    }

    /// A profiles directory that cannot be read, other than one that is not there, is an error: the profiles in it
    /// could not be named, and the server should not start as if there were none.
    fn leftovers(&self, request: LeftoversRequest) -> Result<Leftovers, PluginError> {
        let running: BTreeSet<i64> = request.running.into_iter().collect();
        let mut remove = Vec::new();
        let entries = match std::fs::read_dir(&self.settings.profiles_dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Leftovers::default());
            }
            Err(e) => {
                return Err(PluginError::Failed(format!(
                    "cannot read the profiles directory {}: {e}",
                    self.settings.profiles_dir.display()
                )));
            }
        };
        for entry in entries {
            let path = entry
                .map_err(|e| {
                    PluginError::Failed(format!(
                        "cannot read the profiles directory {}: {e}",
                        self.settings.profiles_dir.display()
                    ))
                })?
                .path();
            let Some(id) = profile_run(&path) else {
                continue;
            };
            if !running.contains(&id) {
                remove.push(path.to_string_lossy().into_owned());
            }
        }
        remove.sort();
        Ok(Leftovers {
            cleanup: Vec::new(),
            remove,
        })
    }
}

fn unchanged(note: impl Into<String>) -> LaunchAnswer {
    LaunchAnswer::Unchanged {
        note: Some(note.into()),
    }
}

/// The run id in the name of a profile file this plugin wrote.
fn profile_run(path: &Path) -> Option<i64> {
    if !crate::commands::is_profile_name(path) {
        return None;
    }
    path.file_name()?
        .to_str()?
        .strip_prefix("run-")?
        .split('-')
        .next()?
        .parse()
        .ok()
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Writes the profile to a new file `run-<id>-<nonce>.json` in `dir`, readable by the owner only, and returns it.
fn write_profile(dir: &Path, run: i64, profile: &Value) -> std::io::Result<PathBuf> {
    make_private_dir(dir)?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let nonce = format!("{:x}{:x}", nanos, COUNTER.fetch_add(1, Ordering::Relaxed));
    let path = dir.join(format!("run-{run}-{nonce}.json"));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(&path)?;
    file.write_all(serde_json::to_string_pretty(profile)?.as_bytes())?;
    Ok(path)
}

fn make_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    std::fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    Ok(())
}

impl Plugin for NonoSandbox {
    fn manifest(&self) -> Manifest {
        Manifest {
            name: NAME.into(),
            version: env!("CARGO_PKG_VERSION").into(),
            kinds: vec![Kind::Launcher],
            project_settings: Vec::new(),
            app_settings: Vec::new(),
            settings_ui: None,
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
                "the nono sandbox (nono {PINNED_NONO_VERSION}) has no method {other:?}"
            ))),
        }
    }
}

// The paths in these tests are Unix paths.
#[cfg(all(test, unix))]
mod tests {
    use std::collections::BTreeMap;

    use moebius_plugin_api::{LaunchRun, Urls};

    use super::*;
    use crate::settings::Network;

    /// The profile key that holds the allowed ports on this platform.
    const PORTS: &str = if cfg!(target_os = "macos") {
        "open_port"
    } else {
        "connect_port"
    };

    fn request(agent: &str) -> LaunchRequest {
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
                ("PATH", "/nonexistent-bin"),
                ("MOEBIUS_RUN_TOKEN", "moebius-run-secret-token"),
                ("OPENAI_API_KEY", "sk-provider"),
            ]
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .into(),
            dir: "/data/worktrees/run-7".into(),
            read_only: vec!["/data/runs/7/agent".into()],
            read_write: vec!["/data/worktrees/run-7".into(), "/data/runs/7/tmp".into()],
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

    fn plugin(dir: &Path, edit: impl FnOnce(&mut Settings)) -> NonoSandbox {
        let mut settings = Settings {
            nono: "/opt/nono/bin/nono".into(),
            profiles_dir: dir.join("profiles"),
            ..Settings::default()
        };
        edit(&mut settings);
        NonoSandbox::new(settings, PathBuf::from("/opt/plugins/nono-sandbox"))
    }

    fn wrapped(answer: LaunchAnswer) -> Wrapped {
        match answer {
            LaunchAnswer::Wrapped(wrapped) => *wrapped,
            other => panic!("not wrapped: {other:?}"),
        }
    }

    fn profile_of(wrapped: &Wrapped) -> Value {
        let at = wrapped.args.iter().position(|a| a == "--profile").unwrap();
        serde_json::from_str(&std::fs::read_to_string(&wrapped.args[at + 1]).unwrap()).unwrap()
    }

    #[test]
    fn a_launch_is_wrapped_in_nono_run_with_the_profile() {
        let dir = tempfile::tempdir().unwrap();
        let wrapped = wrapped(plugin(dir.path(), |_| {}).launch(request("Coder")).unwrap());
        assert_eq!(wrapped.program, "/opt/nono/bin/nono");
        let profile_arg = &wrapped.args[3];
        assert_eq!(
            &wrapped.args[..3],
            ["--silent", "run", "--profile"].map(String::from)
        );
        assert!(profile_arg.starts_with(dir.path().join("profiles").to_str().unwrap()));
        let dashes = wrapped.args.iter().position(|a| a == "--").unwrap();
        assert_eq!(
            &wrapped.args[dashes + 1..],
            ["pi-bolt", "--mode", "rpc"].map(String::from)
        );
        assert_eq!(wrapped.remove, std::slice::from_ref(profile_arg));
        assert_eq!(
            profile_of(&wrapped)["network"][PORTS],
            serde_json::json!([7172])
        );
    }

    #[test]
    fn the_run_token_is_in_the_environment_and_never_in_argv() {
        let dir = tempfile::tempdir().unwrap();
        let wrapped = wrapped(plugin(dir.path(), |_| {}).launch(request("Coder")).unwrap());
        assert_eq!(wrapped.env["MOEBIUS_RUN_TOKEN"], "moebius-run-secret-token");
        assert!(!wrapped.env.contains_key("OPENAI_API_KEY"));
        let everything =
            serde_json::to_string(&(&wrapped.args, &wrapped.stop, &wrapped.cleanup)).unwrap();
        assert!(
            !everything.contains("secret-token") && !everything.contains("sk-provider"),
            "{everything}"
        );
        let file = std::fs::read_to_string(&wrapped.remove[0]).unwrap();
        assert!(
            !file.contains("secret-token"),
            "the token is not written to the profile either"
        );
    }

    #[cfg(unix)] // File modes.
    #[test]
    fn the_profile_and_its_directory_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let wrapped = wrapped(plugin(dir.path(), |_| {}).launch(request("Coder")).unwrap());
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(Path::new(&wrapped.remove[0])), 0o600);
        assert_eq!(mode(&dir.path().join("profiles")), 0o700);
    }

    #[test]
    fn two_launches_of_one_run_get_two_profiles() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = plugin(dir.path(), |_| {});
        let a = wrapped(plugin.launch(request("Coder")).unwrap());
        let b = wrapped(plugin.launch(request("Coder")).unwrap());
        assert_ne!(a.remove, b.remove);
        assert_ne!(a.stop, b.stop);
    }

    #[test]
    fn stop_and_cleanup_are_commands_of_this_program() {
        let dir = tempfile::tempdir().unwrap();
        let wrapped = wrapped(plugin(dir.path(), |_| {}).launch(request("Coder")).unwrap());
        let stop = wrapped.stop.unwrap();
        let cleanup = wrapped.cleanup.unwrap();
        assert_eq!(stop.program, "/opt/plugins/nono-sandbox");
        assert_eq!(
            &stop.args[..2],
            ["stop", "/opt/nono/bin/nono"].map(String::from)
        );
        assert!(stop.args[2].starts_with("moebius-run-7-"));
        assert_eq!(
            cleanup.args,
            ["cleanup".to_string(), wrapped.remove[0].clone()]
        );
        // The session name is the one nono run was given.
        let at = wrapped.args.iter().position(|a| a == "--name").unwrap();
        assert_eq!(wrapped.args[at + 1], stop.args[2]);
    }

    #[test]
    fn the_network_mode_is_per_agent() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = plugin(dir.path(), |s| {
            s.network = Network::Internet;
            s.agent_network.insert("Coder".into(), Network::Moebius);
        });
        let coder = profile_of(&wrapped(plugin.launch(request("Coder")).unwrap()));
        let other = profile_of(&wrapped(plugin.launch(request("Researcher")).unwrap()));
        assert_eq!(coder["network"]["block"], true);
        assert_eq!(other["network"], serde_json::json!({}));
    }

    #[test]
    fn an_excluded_agent_runs_unchanged_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = plugin(dir.path(), |s| {
            s.exclude_agents.insert("Moebius".into());
        });
        let LaunchAnswer::Unchanged { note: Some(note) } =
            plugin.launch(request("Moebius")).unwrap()
        else {
            panic!("not unchanged")
        };
        assert!(
            note.contains("Moebius") && note.contains("exclude_agents"),
            "{note}"
        );
        assert!(!dir.path().join("profiles").exists());
    }

    #[test]
    fn a_run_on_a_runner_is_unchanged_with_a_note() {
        let dir = tempfile::tempdir().unwrap();
        let mut request = request("Coder");
        request.runner = Some("gpu-box".into());
        let LaunchAnswer::Unchanged { note: Some(note) } =
            plugin(dir.path(), |_| {}).launch(request).unwrap()
        else {
            panic!("not unchanged")
        };
        assert!(note.contains("runner"), "{note}");
        assert!(!dir.path().join("profiles").exists());
    }

    #[test]
    fn a_profile_that_cannot_be_written_fails_the_launch() {
        let dir = tempfile::tempdir().unwrap();
        let mut request = request("Coder");
        request.urls.router = "http://10.1.1.1:7172".into();
        let error = plugin(dir.path(), |_| {}).launch(request).unwrap_err();
        assert!(error.to_string().contains("loopback"), "{error}");
        assert!(!dir.path().join("profiles").exists());
    }

    #[test]
    fn leftovers_are_the_profiles_of_runs_that_are_not_running() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = plugin(dir.path(), |_| {});
        let profiles = dir.path().join("profiles");
        std::fs::create_dir(&profiles).unwrap();
        for name in [
            "run-3-aa.json",
            "run-4-bb.json",
            "run-4-cc.json",
            "notes.txt",
            "run-x-1.json",
        ] {
            std::fs::write(profiles.join(name), "{}").unwrap();
        }
        let left = plugin
            .leftovers(LeftoversRequest { running: vec![4] })
            .unwrap();
        assert_eq!(
            left.remove,
            [profiles
                .join("run-3-aa.json")
                .to_string_lossy()
                .into_owned()]
        );
        assert!(left.cleanup.is_empty());
        let all = plugin.leftovers(LeftoversRequest::default()).unwrap();
        assert_eq!(all.remove.len(), 3);
    }

    #[test]
    fn nono_keeps_no_audit_or_session_record_of_the_launch() {
        let dir = tempfile::tempdir().unwrap();
        let wrapped = wrapped(plugin(dir.path(), |_| {}).launch(request("Coder")).unwrap());
        let before_dashes = &wrapped.args[..wrapped.args.iter().position(|a| a == "--").unwrap()];
        for flag in ["--no-audit", "--no-rollback"] {
            assert!(before_dashes.contains(&flag.to_string()), "{flag}");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_note_says_the_ports_are_open_on_every_host() {
        let dir = tempfile::tempdir().unwrap();
        let wrapped = wrapped(plugin(dir.path(), |_| {}).launch(request("Coder")).unwrap());
        assert!(wrapped.note.unwrap().contains("every host"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_note_says_when_unix_sockets_are_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = plugin(dir.path(), |s| {
            s.unix_sockets = crate::settings::UnixSockets::Allow
        });
        let note = wrapped(plugin.launch(request("Coder")).unwrap())
            .note
            .unwrap();
        assert!(
            note.contains("every host") && note.contains("not mediated"),
            "{note}"
        );
    }

    #[cfg(unix)] // Permissions of a directory.
    #[test]
    fn an_unreadable_profiles_directory_is_an_error_not_an_empty_answer() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let profiles = dir.path().join("profiles");
        std::fs::create_dir(&profiles).unwrap();
        std::fs::set_permissions(&profiles, std::fs::Permissions::from_mode(0o000)).unwrap();
        let readable = std::fs::read_dir(&profiles).is_ok(); // Running as root reads it anyway.
        let result = plugin(dir.path(), |_| {}).leftovers(LeftoversRequest::default());
        std::fs::set_permissions(&profiles, std::fs::Permissions::from_mode(0o700)).unwrap();
        if !readable {
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("profiles directory")
            );
        }
        // A path that is a file is not a directory: also an error, whoever runs this.
        let file = dir.path().join("file");
        std::fs::write(&file, "").unwrap();
        let result = plugin(dir.path(), |s| s.profiles_dir = file.clone())
            .leftovers(LeftoversRequest::default());
        assert!(result.is_err());
    }

    #[test]
    fn leftovers_without_a_profiles_directory_are_none() {
        let dir = tempfile::tempdir().unwrap();
        let left = plugin(dir.path(), |_| {})
            .leftovers(LeftoversRequest::default())
            .unwrap();
        assert!(left.remove.is_empty());
    }

    #[test]
    fn the_manifest_is_a_launcher_and_other_methods_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = plugin(dir.path(), |_| {});
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
