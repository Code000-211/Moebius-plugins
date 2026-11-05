//! The nono profile of one run, as JSON data, and the environment the run keeps.
//!
//! The profile is a minimal profile of its own, not an extension of a nono pack: the packs grant the user's
//! configuration directories and caches, which a Moebius run has no use for. nono adds base groups to every profile
//! whether the profile names them or not, so the profile excludes the ones that grant more than a run needs
//! (`system_write_linux` and `system_write_macos` write access to `/tmp`, `user_tools`, `homebrew_*`) and grants
//! the devices a program needs by name. On Linux it includes nono's system read group, and on macOS the system
//! directories listed in [`MACOS_READ`]. It adds exactly:
//!
//! - read and write: the request's `read_write` paths and the working directory (the worktree, the agent
//!   directory, the sessions directory, the run's temporary directory), and the devices of [`DEVICES`];
//! - read only: the request's `read_only` paths (the extensions), the clone whose git directory
//!   the worktree uses, the program a run operates the server with (`MOEBIUS`), the directory of the harness's installation, the directories on the run's `PATH` that exist
//!   (the toolchains), the rustup and cargo directories a toolchain on `PATH` needs, and the `read_only` setting;
//! - the network: in mode `moebius` all outbound access is blocked except TCP to the ports of the request's router,
//!   gateway and check addresses, and of the desktop API for a run that operates the server (`MOEBIUS_URL`), which must
//!   be on the loopback; in mode `internet` outbound access is not
//!   restricted. On Linux the ports are `connect_port` rules, which Landlock applies to every host (see
//!   [`network_note`]); on macOS `open_port`, which Seatbelt limits to localhost;
//! - Unix sockets: `linux.af_unix_mediation` is `pathname` and no socket is granted, so a run cannot connect to the
//!   D-Bus, Docker or ssh-agent sockets of its user. With the setting `unix_sockets` at `allow` the key is left out
//!   and nono does not mediate them (see [`socket_note`]);
//! - the environment: only the variables of [`kept_env`].
//!
//! A directory that is granted for reading by the environment (a `PATH` entry, the harness's directory, the setting
//! `read_only`) and that contains the worktree, the clone, another path the run writes or the profiles directory
//! fails the launch: it would make the data directory, its secrets and the other worktrees readable too. Nothing else
//! is granted, so the data directory, `secrets.toml`, the token file and `moebius.toml` are out of reach.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use moebius_plugin_api::{LaunchRequest, Urls};
use serde_json::{Value, json};

use crate::settings::{Network, Settings, UnixSockets};

/// Why a profile cannot be written for a request. The launch then fails, which fails the run: a sandbox that cannot
/// be described is never skipped.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProfileError {
    #[error("the path {0:?} is not absolute; the sandbox only grants absolute paths")]
    Relative(String),
    #[error("the path {0:?} contains $ or *, which nono would expand as a variable or a pattern")]
    Special(String),
    #[error("cannot read the host and port of the address {0:?}")]
    BadUrl(String),
    #[error(
        "the address {0:?} is not on this machine's loopback; mode \"moebius\" can only allow loopback ports, since \
         nono filters TCP by port, not by host. Use mode \"internet\" for this agent or serve the router on 127.0.0.1"
    )]
    NotLoopback(String),
    #[error(
        "the address {url:?} uses port {port}, a well-known port; mode \"moebius\" would then allow connecting to \
         port {port} of every host (nono filters TCP by port, not by host). Serve the router on a port of 1024 or \
         more, or use mode \"internet\" for this agent"
    )]
    WellKnownPort { url: String, port: u16 },
    #[error(
        "the directory {grant:?} ({origin}) would be readable by the run, and it contains {inner:?}, so the data \
         directory, its secrets and other runs' worktrees would be readable too. Remove it from the PATH or the \
         setting, or move {inner:?} out of it"
    )]
    Exposes {
        grant: String,
        origin: String,
        inner: String,
    },
}

/// The names of the environment variables a run keeps; others are dropped, provider keys among them. Besides the
/// basics: the proxy and certificate variables of a host behind a proxy, the toolchain homes, and the harness's
/// agent directory.
const KEPT: [&str; 25] = [
    "PATH",
    "HOME",
    "USER",
    "SHELL",
    "LANG",
    "LANGUAGE",
    "TERM",
    "TZ",
    "TMPDIR",
    "TEMP",
    "TMP",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "NODE_EXTRA_CA_CERTS",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "PI_CODING_AGENT_DIR",
];

/// Whether a run keeps the variable: the usual few, the harness's, `LC_*`, and Moebius's own (`MOEBIUS`,
/// `MOEBIUS_*`: the run token, the check URL and the like).
fn kept(name: &str) -> bool {
    KEPT.contains(&name)
        || name.starts_with("LC_")
        || name == "MOEBIUS"
        || name.starts_with("MOEBIUS_")
}

/// The environment of the wrapped program: the request's, without what the run does not need. This is the process
/// environment of `nono`, never its command line, so the run token is not in argv.
pub fn kept_env(env: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    env.iter()
        .filter(|(name, _)| kept(name))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect()
}

/// The loopback ports of the three addresses, sorted and without repeats. A port below 1024 is refused: nono allows
/// a port for every host, and the web's ports would be the whole internet.
pub fn loopback_ports(urls: &Urls) -> Result<Vec<u16>, ProfileError> {
    let mut ports = BTreeSet::new();
    for url in [&urls.router, &urls.gateway, &urls.check] {
        ports.insert(loopback_port(url)?);
    }
    Ok(ports.into_iter().collect())
}

/// The ports a run may connect to: those of [`loopback_ports`] and, for a run that operates the server with
/// `"$MOEBIUS"` (the Moebius agent), the port of the desktop API that the request's `MOEBIUS_URL` names. The same
/// rules hold for it: loopback, and 1024 or more.
pub fn allowed_ports(request: &LaunchRequest) -> Result<Vec<u16>, ProfileError> {
    let mut ports: BTreeSet<u16> = loopback_ports(&request.urls)?.into_iter().collect();
    if let Some(url) = request.env.get(DESKTOP_API) {
        ports.insert(loopback_port(url)?);
    }
    Ok(ports.into_iter().collect())
}

/// The variable that names the desktop API for the runs that operate the server.
const DESKTOP_API: &str = "MOEBIUS_URL";

fn loopback_port(url: &str) -> Result<u16, ProfileError> {
    let (host, port) = host_and_port(url)?;
    let loopback = host == "localhost" || host == "::1" || host.starts_with("127.");
    if !loopback {
        return Err(ProfileError::NotLoopback(url.to_string()));
    }
    if port < 1024 {
        return Err(ProfileError::WellKnownPort {
            url: url.to_string(),
            port,
        });
    }
    Ok(port)
}

fn host_and_port(url: &str) -> Result<(String, u16), ProfileError> {
    let bad = || ProfileError::BadUrl(url.to_string());
    let (scheme, rest) = url.split_once("://").ok_or_else(bad)?;
    let default = match scheme {
        "http" => 80,
        "https" => 443,
        _ => return Err(bad()),
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains('@') {
        return Err(bad());
    }
    let (host, port) = if let Some(inner) = authority.strip_prefix('[') {
        let (host, after) = inner.split_once(']').ok_or_else(bad)?;
        (host, after.strip_prefix(':'))
    } else {
        match authority.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        }
    };
    if host.is_empty() {
        return Err(bad());
    }
    let port = match port {
        Some(text) => text.parse().map_err(|_| bad())?,
        None => default,
    };
    Ok((host.to_ascii_lowercase(), port))
}

/// The directories of the harness's installation: where the program is and, if it is a link, where it points. A
/// program in a `node_modules` directory is an npm package, so its package directory is granted whole.
pub fn program_dirs(program: &str, path_var: Option<&str>) -> Vec<PathBuf> {
    let Some(found) = find_program(program, path_var) else {
        return Vec::new();
    };
    let mut dirs = Vec::new();
    if let Some(parent) = found.parent() {
        dirs.push(parent.to_path_buf());
    }
    if let Ok(real) = found.canonicalize() {
        let components: Vec<_> = real.components().collect();
        let package = components
            .iter()
            .rposition(|c| c.as_os_str() == "node_modules");
        match package {
            Some(index) if index + 1 < components.len() => {
                let scoped = components[index + 1]
                    .as_os_str()
                    .to_string_lossy()
                    .starts_with('@');
                let end = (index + if scoped { 3 } else { 2 }).min(components.len());
                dirs.push(components[..end].iter().collect());
            }
            _ => dirs.extend(real.parent().map(Path::to_path_buf)),
        }
    }
    dirs
}

fn find_program(program: &str, path_var: Option<&str>) -> Option<PathBuf> {
    if program.contains('/') || program.contains(std::path::MAIN_SEPARATOR) {
        return Some(PathBuf::from(program));
    }
    std::env::split_paths(path_var?)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

fn check_path(path: &Path) -> Result<String, ProfileError> {
    let text = path.to_string_lossy().into_owned();
    if !path.is_absolute() {
        return Err(ProfileError::Relative(text));
    }
    if text.contains('$') || text.contains('*') {
        return Err(ProfileError::Special(text));
    }
    Ok(text)
}

/// Writes the grants of `paths` into `dirs` and `files`, by what each path is on disk (a path that does not exist is
/// taken for a directory).
fn sort_grants(
    paths: impl IntoIterator<Item = PathBuf>,
    dirs: &mut Vec<String>,
    files: &mut Vec<String>,
) -> Result<(), ProfileError> {
    for path in paths {
        let text = check_path(&path)?;
        let list = if path.is_file() {
            &mut *files
        } else {
            &mut *dirs
        };
        if !list.contains(&text) {
            list.push(text);
        }
    }
    Ok(())
}

/// The groups nono adds to every profile that this profile does not want: write access to `/tmp` and the like, the
/// user's tool directories and Homebrew.
const EXCLUDED_GROUPS: [&str; 5] = [
    "system_write_linux",
    "system_write_macos",
    "user_tools",
    "homebrew_linux",
    "homebrew_macos",
];

/// The devices a program needs, granted by name since the write group that held them is excluded.
const DEVICES: [&str; 4] = ["/dev/null", "/dev/zero", "/dev/full", "/dev/tty"];
/// The directories of open file descriptors (`<(...)` and the like).
const FD_DIRS: [&str; 2] = ["/dev/fd", "/proc/self/fd"];

/// What a program needs to start on macOS, read only, in place of the group `system_read_macos`, which also reads
/// `/opt`, `/var`, `/private`, `/tmp`, `/etc`, `/Library`, `/Applications`, `/nix` and `/run`. Untested on macOS.
const MACOS_READ: [&str; 6] = [
    "/usr",
    "/bin",
    "/sbin",
    "/System",
    "/Library/Frameworks",
    "/private/etc/ssl",
];
const MACOS_DEVICES: [&str; 3] = ["/dev/null", "/dev/zero", "/dev/tty"];
const MACOS_READ_DEVICES: [&str; 2] = ["/dev/random", "/dev/urandom"];

/// The cargo and rustup directories a run needs when its `PATH` holds a toolchain from either: rustup's home whole
/// (the toolchains) and cargo's downloaded sources. Not the rest of cargo's home, which holds the registry token.
fn toolchain_dirs(env: &BTreeMap<String, String>, path_dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut homes: Vec<PathBuf> = Vec::new();
    if let Some(home) = env.get("HOME") {
        homes.push(PathBuf::from(home));
    }
    homes.extend(std::env::var_os("HOME").map(PathBuf::from));
    let named = |variable: &str, default: &str| -> Vec<PathBuf> {
        match env.get(variable) {
            Some(dir) => vec![PathBuf::from(dir)],
            None => homes.iter().map(|home| home.join(default)).collect(),
        }
    };
    let cargo = named("CARGO_HOME", ".cargo");
    let rustup = named("RUSTUP_HOME", ".rustup");
    let used = cargo
        .iter()
        .chain(&rustup)
        .any(|home| path_dirs.iter().any(|dir| dir.starts_with(home)));
    if !used {
        return Vec::new();
    }
    let mut dirs: Vec<PathBuf> = rustup;
    for home in cargo {
        dirs.push(home.join("registry"));
        dirs.push(home.join("git"));
    }
    dirs.retain(|dir| dir.is_absolute() && dir.is_dir());
    dirs
}

/// Whether `inner` is inside `grant` (or is it, when `or_equal`), compared as written and, where the paths exist,
/// with links resolved.
fn contains(grant: &Path, inner: &Path, or_equal: bool) -> bool {
    let within =
        |grant: &Path, inner: &Path| inner.starts_with(grant) && (or_equal || inner != grant);
    if within(grant, inner) {
        return true;
    }
    match (grant.canonicalize(), inner.canonicalize()) {
        (Ok(grant), Ok(inner)) => within(&grant, &inner),
        _ => false,
    }
}

/// The profile of the run in `request`, with the network mode `mode`, for the platform `os`
/// (`std::env::consts::OS`).
pub fn build(
    request: &LaunchRequest,
    settings: &Settings,
    mode: Network,
    os: &str,
) -> Result<Value, ProfileError> {
    let macos = os == "macos";
    let (mut write_dirs, mut write_files) = (Vec::new(), Vec::new());
    let writable = request
        .read_write
        .iter()
        .chain([&request.dir])
        .map(PathBuf::from);
    sort_grants(writable, &mut write_dirs, &mut write_files)?;
    let own: Vec<PathBuf> = request
        .read_write
        .iter()
        .chain([&request.dir])
        .chain(&request.read_only)
        .chain(&request.clone)
        .map(PathBuf::from)
        .collect();

    // What the request names is the run's own. What the environment and the settings name is checked below.
    let mut readable: Vec<PathBuf> = request.read_only.iter().map(PathBuf::from).collect();
    readable.extend(request.clone.iter().map(PathBuf::from));
    // The program a run operates the server with (`"$MOEBIUS"`), which is the one file it needs of the server.
    readable.extend(
        request
            .env
            .get("MOEBIUS")
            .map(PathBuf::from)
            .filter(|program| program.is_absolute() && program.is_file()),
    );
    let path_var = request.env.get("PATH").map(String::as_str);
    // Relative and empty entries are left out: granting less is safe.
    let path_dirs: Vec<PathBuf> = path_var
        .map(|var| {
            std::env::split_paths(var)
                .filter(|dir| dir.is_absolute() && dir.is_dir())
                .collect()
        })
        .unwrap_or_default();
    let mut environmental: Vec<(String, PathBuf)> = Vec::new();
    environmental.extend(
        settings
            .read_only
            .iter()
            .map(|dir| ("the setting read_only".to_string(), dir.clone())),
    );
    environmental.extend(
        program_dirs(&request.program, path_var)
            .into_iter()
            .map(|dir| (format!("the directory of {}", request.program), dir)),
    );
    environmental.extend(
        path_dirs
            .iter()
            .map(|dir| ("an entry of the run's PATH".to_string(), dir.clone())),
    );
    environmental.extend(
        toolchain_dirs(&request.env, &path_dirs)
            .into_iter()
            .map(|dir| ("a toolchain directory".to_string(), dir)),
    );
    if macos {
        readable.extend(MACOS_READ.iter().map(PathBuf::from));
    }
    let protected: Vec<(PathBuf, bool)> = request
        .read_write
        .iter()
        .chain([&request.dir])
        .chain(&request.clone)
        .map(|path| (PathBuf::from(path), false))
        .chain([(settings.profiles_dir.clone(), true)])
        .collect();
    for (source, grant) in &environmental {
        if own.contains(grant) {
            continue;
        }
        for (inner, or_equal) in &protected {
            if contains(grant, inner, *or_equal) {
                return Err(ProfileError::Exposes {
                    grant: grant.to_string_lossy().into_owned(),
                    origin: source.clone(),
                    inner: inner.to_string_lossy().into_owned(),
                });
            }
        }
    }
    readable.extend(environmental.into_iter().map(|(_, dir)| dir));
    let (mut read_dirs, mut read_files) = (Vec::new(), Vec::new());
    sort_grants(readable, &mut read_dirs, &mut read_files)?;
    read_dirs.retain(|path| !write_dirs.contains(path));
    read_files.retain(|path| !write_files.contains(path));

    // The devices and descriptor directories, which the excluded write groups used to grant.
    let (devices, fd_dirs): (&[&str], &[&str]) = if macos {
        (&MACOS_DEVICES, &FD_DIRS[..1])
    } else {
        (&DEVICES, &FD_DIRS)
    };
    write_files.extend(devices.iter().map(|d| d.to_string()));
    write_dirs.extend(fd_dirs.iter().map(|d| d.to_string()));
    if macos {
        read_files.extend(MACOS_READ_DEVICES.iter().map(|d| d.to_string()));
    }

    let mut filesystem = serde_json::Map::new();
    for (key, list) in [
        ("allow", write_dirs),
        ("allow_file", write_files),
        ("read", read_dirs),
        ("read_file", read_files),
    ] {
        if !list.is_empty() {
            filesystem.insert(key.into(), json!(list));
        }
    }

    let network = match mode {
        Network::Moebius => {
            let ports = allowed_ports(request)?;
            if macos {
                json!({"block": true, "open_port": ports})
            } else {
                json!({"block": true, "connect_port": ports})
            }
        }
        Network::Internet => json!({}),
    };
    let env: Vec<String> = kept_env(&request.env).into_keys().collect();

    let mut excluded: Vec<&str> = EXCLUDED_GROUPS.to_vec();
    let mut groups = serde_json::Map::new();
    if macos {
        excluded.push("system_read_macos");
    } else {
        groups.insert(
            "include".into(),
            json!([{"name": "system_read_linux_core", "when": "linux"}]),
        );
    }
    groups.insert("exclude".into(), json!(excluded));

    let mut profile = json!({
        "meta": {
            "name": format!("moebius-run-{}", request.run.id),
            "description": format!(
                "Moebius run {} ({}, agent {}), written by the nono-sandbox plugin",
                request.run.id, request.run.purpose, request.run.agent
            ),
        },
        "groups": groups,
        "filesystem": filesystem,
        "workdir": {"access": "readwrite"},
        "network": network,
        "environment": {"allow_vars": env},
    });
    if settings.unix_sockets == UnixSockets::Deny {
        profile["linux"] = json!({"af_unix_mediation": "pathname"});
    }
    Ok(profile)
}

/// A sentence for the run's record when the sandbox is weaker than its name says: on Linux nono filters TCP by port,
/// so in mode `moebius` the ports of the router, gateway and check address are open on every host.
pub fn network_note(request: &LaunchRequest, mode: Network, os: &str) -> Option<String> {
    if mode != Network::Moebius || os == "macos" {
        return None;
    }
    let ports = allowed_ports(request).ok()?;
    let ports: Vec<String> = ports.iter().map(u16::to_string).collect();
    Some(format!(
        "nono filters TCP by port on Linux, not by host: the run can connect to port {} of every host, not only of \
         the loopback",
        ports.join(", ")
    ))
}

/// A sentence for the run's record when nono does not mediate Unix sockets, which only nono on Linux does.
pub fn socket_note(settings: &Settings, os: &str) -> Option<String> {
    (settings.unix_sockets == UnixSockets::Allow && os == "linux").then(|| {
        "Unix sockets are not mediated (unix_sockets = allow): the run can connect to any socket its user can reach \
         by path, such as the Docker, D-Bus or ssh-agent sockets or, on WSL2, the Windows interop socket, and so \
         act as its user outside the sandbox"
            .to_string()
    })
}

// The paths in these tests are Unix paths.
#[cfg(all(test, unix))]
mod tests {
    use moebius_plugin_api::LaunchRun;

    use super::*;

    pub(crate) fn request() -> LaunchRequest {
        LaunchRequest {
            run: LaunchRun {
                id: 7,
                purpose: "code".into(),
                agent: "Coder".into(),
                chat: false,
                card: Some(3),
            },
            program: "/opt/pi-bolt/bin/pi-bolt".into(),
            args: vec!["--mode".into(), "rpc".into()],
            env: BTreeMap::from([
                ("PATH".to_string(), "/nonexistent-bin".to_string()),
                ("HOME".to_string(), "/home/runner".to_string()),
                (
                    "MOEBIUS_RUN_TOKEN".to_string(),
                    "moebius-run-secret".to_string(),
                ),
                (
                    "MOEBIUS_CHECK_URL".to_string(),
                    "http://127.0.0.1:7172".to_string(),
                ),
                ("OPENAI_API_KEY".to_string(), "sk-provider".to_string()),
                ("ANTHROPIC_API_KEY".to_string(), "sk-other".to_string()),
                ("TMPDIR".to_string(), "/data/runs/7/tmp".to_string()),
            ]),
            dir: "/data/worktrees/run-7".into(),
            read_only: vec!["/data/extensions/bridge".into()],
            read_write: vec![
                "/data/worktrees/run-7".into(),
                "/data/runs/7/agent".into(),
                "/data/runs/7/sessions".into(),
                "/data/runs/7/tmp".into(),
            ],
            urls: Urls {
                router: "http://127.0.0.1:7172".into(),
                gateway: "http://127.0.0.1:7172/mcp".into(),
                check: "http://127.0.0.1:7172".into(),
            },
            clone: Some("/data/repos/demo".into()),
            runner: None,
        }
    }

    fn strings(profile: &Value, section: &str, key: &str) -> Vec<String> {
        profile[section][key]
            .as_array()
            .map(|a| a.iter().map(|v| v.as_str().unwrap().to_string()).collect())
            .unwrap_or_default()
    }

    /// Every path the profile grants, in any way.
    fn grants(profile: &Value) -> Vec<String> {
        [
            "allow",
            "allow_file",
            "read",
            "read_file",
            "write",
            "write_file",
        ]
        .iter()
        .flat_map(|key| strings(profile, "filesystem", key))
        .collect()
    }

    #[test]
    fn the_run_writes_where_it_works_and_reads_what_it_is_given() {
        let profile = build(&request(), &Settings::default(), Network::Moebius, "linux").unwrap();
        assert_eq!(
            strings(&profile, "filesystem", "allow"),
            [
                "/data/worktrees/run-7",
                "/data/runs/7/agent",
                "/data/runs/7/sessions",
                "/data/runs/7/tmp",
                "/dev/fd",
                "/proc/self/fd"
            ]
        );
        let read = strings(&profile, "filesystem", "read");
        for path in [
            "/data/extensions/bridge",
            "/data/repos/demo",
            "/opt/pi-bolt/bin",
        ] {
            assert!(read.contains(&path.to_string()), "{path} in {read:?}");
        }
        assert_eq!(profile["workdir"]["access"], "readwrite");
    }

    #[test]
    fn nothing_grants_the_data_directory_or_its_secrets() {
        let profile = build(&request(), &Settings::default(), Network::Moebius, "linux").unwrap();
        for secret in [
            "/data",
            "/data/secrets.toml",
            "/data/token",
            "/data/moebius.toml",
            "/data/store.db",
        ] {
            let secret = Path::new(secret);
            // A grant covers a path when the grant is the path or one of its parents.
            let covering: Vec<_> = grants(&profile)
                .into_iter()
                .filter(|grant| secret.starts_with(grant))
                .collect();
            assert!(covering.is_empty(), "{secret:?} is covered by {covering:?}");
        }
    }

    #[test]
    fn settings_add_read_only_paths_and_the_path_directories() {
        let dir = tempfile::tempdir().unwrap();
        let tools = dir.path().join("tools");
        std::fs::create_dir(&tools).unwrap();
        let mut request = request();
        request.env.insert(
            "PATH".into(),
            std::env::join_paths([tools.clone(), PathBuf::from("/nonexistent-bin")])
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        );
        let settings = Settings {
            read_only: vec![PathBuf::from("/opt/sdk")],
            ..Settings::default()
        };
        let profile = build(&request, &settings, Network::Moebius, "linux").unwrap();
        let read = strings(&profile, "filesystem", "read");
        assert!(read.contains(&tools.to_string_lossy().into_owned()));
        assert!(read.contains(&"/opt/sdk".to_string()));
        assert!(!read.contains(&"/nonexistent-bin".to_string()));
    }

    #[test]
    fn a_file_is_granted_as_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("models.json");
        std::fs::write(&file, "{}").unwrap();
        let mut request = request();
        request.read_only.push(file.to_string_lossy().into_owned());
        let profile = build(&request, &Settings::default(), Network::Moebius, "linux").unwrap();
        assert_eq!(
            strings(&profile, "filesystem", "read_file"),
            [file.to_string_lossy()]
        );
    }

    #[test]
    fn an_npm_package_is_granted_whole() {
        let dir = tempfile::tempdir().unwrap();
        let package = dir.path().join("lib/node_modules/@scope/pi-bolt");
        std::fs::create_dir_all(package.join("bin")).unwrap();
        std::fs::write(package.join("bin/pi-bolt.js"), "").unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(package.join("bin/pi-bolt.js"), bin.join("pi-bolt")).unwrap();
        #[cfg(unix)]
        {
            let dirs = program_dirs("pi-bolt", Some(&bin.to_string_lossy()));
            assert!(dirs.contains(&bin));
            assert!(dirs.contains(&package.canonicalize().unwrap()), "{dirs:?}");
        }
    }

    #[test]
    fn mode_moebius_blocks_the_network_but_for_the_loopback_ports() {
        let mut request = request();
        request.urls.gateway = "http://localhost:7173/mcp".into();
        request.urls.check = "http://[::1]:7174".into();
        let profile = build(&request, &Settings::default(), Network::Moebius, "linux").unwrap();
        assert_eq!(profile["network"]["block"], true);
        assert_eq!(
            profile["network"]["connect_port"],
            json!([7172, 7173, 7174])
        );
        assert!(profile["network"].get("open_port").is_none());
        assert!(profile["network"].get("allow_domain").is_none());
    }

    #[test]
    fn mode_internet_does_not_restrict_outbound_access() {
        let profile = build(&request(), &Settings::default(), Network::Internet, "linux").unwrap();
        assert_eq!(profile["network"], json!({}));
    }

    #[test]
    fn mode_moebius_refuses_an_address_it_cannot_filter() {
        let mut request = request();
        request.urls.router = "http://10.0.0.1:7172".into();
        let error = build(&request, &Settings::default(), Network::Moebius, "linux").unwrap_err();
        assert_eq!(
            error,
            ProfileError::NotLoopback("http://10.0.0.1:7172".into())
        );
        assert!(build(&request, &Settings::default(), Network::Internet, "linux").is_ok());
        request.urls.router = "7172".into();
        assert!(matches!(
            build(&request, &Settings::default(), Network::Moebius, "linux"),
            Err(ProfileError::BadUrl(_))
        ));
    }

    #[test]
    fn a_run_that_operates_the_server_may_reach_the_desktop_api_and_read_its_program() {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("moebius");
        std::fs::write(&program, "").unwrap();
        let mut request = request();
        request
            .env
            .insert("MOEBIUS_URL".into(), "http://127.0.0.1:7171".into());
        request
            .env
            .insert("MOEBIUS".into(), program.to_string_lossy().into_owned());
        let profile = build(&request, &Settings::default(), Network::Moebius, "linux").unwrap();
        assert_eq!(
            profile["network"]["connect_port"],
            json!([7171, 7172]),
            "{profile}"
        );
        assert!(
            strings(&profile, "filesystem", "read_file")
                .contains(&program.to_string_lossy().into_owned())
        );
        // A desktop API that is not on the loopback is refused, as the router is.
        request
            .env
            .insert("MOEBIUS_URL".into(), "http://10.0.0.5:7171".into());
        assert!(matches!(
            build(&request, &Settings::default(), Network::Moebius, "linux"),
            Err(ProfileError::NotLoopback(_))
        ));
    }

    #[test]
    fn well_known_ports_are_refused_in_mode_moebius() {
        for url in [
            "https://localhost",
            "http://127.0.0.1/mcp",
            "http://127.0.0.1:1023",
        ] {
            let urls = Urls {
                router: url.into(),
                gateway: "http://127.0.0.1:7172/mcp".into(),
                check: "http://127.0.0.1:8080".into(),
            };
            assert!(
                matches!(
                    loopback_ports(&urls),
                    Err(ProfileError::WellKnownPort { .. })
                ),
                "{url}"
            );
        }
        let mut request = request();
        request.urls.router = "https://localhost".into();
        let error = build(&request, &Settings::default(), Network::Moebius, "linux").unwrap_err();
        assert!(error.to_string().contains("443"), "{error}");
        // Mode internet restricts nothing, so it needs no port.
        assert!(build(&request, &Settings::default(), Network::Internet, "linux").is_ok());
    }

    #[test]
    fn the_environment_keeps_what_the_run_needs_and_no_provider_keys() {
        let kept = kept_env(&request().env);
        assert_eq!(
            kept.keys().map(String::as_str).collect::<Vec<_>>(),
            [
                "HOME",
                "MOEBIUS_CHECK_URL",
                "MOEBIUS_RUN_TOKEN",
                "PATH",
                "TMPDIR"
            ]
        );
        let profile = build(&request(), &Settings::default(), Network::Moebius, "linux").unwrap();
        let allowed = strings(&profile, "environment", "allow_vars");
        assert_eq!(allowed, kept.keys().cloned().collect::<Vec<_>>());
    }

    #[test]
    fn the_profile_never_holds_the_run_token_or_a_key() {
        let profile = build(&request(), &Settings::default(), Network::Moebius, "linux")
            .unwrap()
            .to_string();
        assert!(
            !profile.contains("moebius-run-secret") && !profile.contains("sk-"),
            "{profile}"
        );
    }

    #[test]
    fn paths_nono_would_expand_or_that_are_relative_are_refused() {
        let mut request = request();
        request.read_only.push("/data/$HOME/x".into());
        assert!(matches!(
            build(&request, &Settings::default(), Network::Moebius, "linux"),
            Err(ProfileError::Special(_))
        ));
        request.read_only = vec!["/data/**".into()];
        assert!(matches!(
            build(&request, &Settings::default(), Network::Moebius, "linux"),
            Err(ProfileError::Special(_))
        ));
        request.read_only = vec!["agent".into()];
        assert!(matches!(
            build(&request, &Settings::default(), Network::Moebius, "linux"),
            Err(ProfileError::Relative(_))
        ));
    }

    #[test]
    fn the_groups_that_write_to_tmp_and_the_tool_directories_are_excluded() {
        let profile = build(&request(), &Settings::default(), Network::Moebius, "linux").unwrap();
        let excluded = strings(&profile, "groups", "exclude");
        for group in [
            "system_write_linux",
            "system_write_macos",
            "user_tools",
            "homebrew_linux",
            "homebrew_macos",
        ] {
            assert!(excluded.contains(&group.to_string()), "{group}");
        }
        let included = profile["groups"]["include"].to_string();
        assert!(included.contains("system_read_linux_core"));
        assert!(!included.contains("system_write"), "{included}");
        // The devices are granted by name instead, and nothing else of /dev or /tmp.
        let files = strings(&profile, "filesystem", "allow_file");
        for device in ["/dev/null", "/dev/zero", "/dev/full", "/dev/tty"] {
            assert!(files.contains(&device.to_string()), "{device}");
        }
        for path in grants(&profile) {
            assert!(
                !Path::new("/tmp").starts_with(&path) || path == "/",
                "{path} covers /tmp"
            );
            assert!(!path.starts_with("/tmp"), "{path}");
        }
    }

    #[test]
    fn unix_sockets_are_mediated_and_none_is_granted() {
        for os in ["linux", "macos"] {
            let profile = build(&request(), &Settings::default(), Network::Internet, os).unwrap();
            assert_eq!(profile["linux"]["af_unix_mediation"], "pathname");
            assert!(grants(&profile).iter().all(|p| !p.contains(".sock")));
            assert!(profile["filesystem"].get("unix_socket").is_none());
        }
        assert_eq!(socket_note(&Settings::default(), "linux"), None);
    }

    #[test]
    fn allowed_unix_sockets_leave_mediation_out_and_say_so() {
        let settings = Settings {
            unix_sockets: UnixSockets::Allow,
            ..Settings::default()
        };
        let profile = build(&request(), &settings, Network::Moebius, "linux").unwrap();
        assert!(profile.get("linux").is_none(), "{profile}");
        // The rest of the sandbox stays.
        assert_eq!(profile["network"]["block"], true);
        assert!(!strings(&profile, "filesystem", "allow").is_empty());
        assert!(
            socket_note(&settings, "linux")
                .unwrap()
                .contains("not mediated")
        );
        assert_eq!(socket_note(&settings, "macos"), None);
    }

    #[test]
    fn on_macos_the_system_read_group_is_replaced_by_a_short_list() {
        let profile = build(&request(), &Settings::default(), Network::Moebius, "macos").unwrap();
        assert!(strings(&profile, "groups", "exclude").contains(&"system_read_macos".to_string()));
        assert!(profile["groups"].get("include").is_none());
        let read = strings(&profile, "filesystem", "read");
        for path in ["/usr", "/bin", "/sbin", "/System", "/Library/Frameworks"] {
            assert!(read.contains(&path.to_string()), "{path}");
        }
        for path in [
            "/opt", "/var", "/private", "/tmp", "/etc", "/Library", "/nix",
        ] {
            assert!(!read.contains(&path.to_string()), "{path}");
        }
        assert_eq!(profile["network"]["open_port"], json!([7172]));
        assert!(profile["network"].get("connect_port").is_none());
    }

    #[test]
    fn a_path_entry_that_holds_the_data_directory_fails_the_launch() {
        let mut request = request();
        request.env.insert("PATH".into(), "/data".into());
        // The directory must exist to be on the path; /data is not there, so use one that holds the worktree.
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path().join("data/worktrees/run-7");
        std::fs::create_dir_all(&worktree).unwrap();
        request.dir = worktree.to_string_lossy().into_owned();
        request.read_write = vec![request.dir.clone()];
        request.clone = None;
        request
            .env
            .insert("PATH".into(), dir.path().to_string_lossy().into_owned());
        let error = build(&request, &Settings::default(), Network::Internet, "linux").unwrap_err();
        assert!(matches!(error, ProfileError::Exposes { .. }), "{error}");
        let text = error.to_string();
        assert!(
            text.contains("PATH") && text.contains(&*dir.path().to_string_lossy()),
            "{text}"
        );
    }

    #[test]
    fn the_read_only_setting_and_the_harness_directory_are_checked_too() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let worktree = data.join("worktrees/run-7");
        std::fs::create_dir_all(&worktree).unwrap();
        let mut request = request();
        request.dir = worktree.to_string_lossy().into_owned();
        request.read_write = vec![request.dir.clone()];
        request.clone = None;
        let settings = Settings {
            read_only: vec![dir.path().to_path_buf()],
            ..Settings::default()
        };
        let error = build(&request, &settings, Network::Internet, "linux").unwrap_err();
        assert!(error.to_string().contains("read_only"), "{error}");
        // Pi-Bolt installed next to the data directory.
        let bin = data.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("pi-bolt"), "").unwrap();
        request.program = bin.join("pi-bolt").to_string_lossy().into_owned();
        let tight = Settings {
            read_only: vec![bin.clone()],
            ..Settings::default()
        };
        assert!(build(&request, &tight, Network::Internet, "linux").is_ok());
        request.program = dir.path().join("pi-bolt").to_string_lossy().into_owned();
        std::fs::write(&request.program, "").unwrap();
        let error = build(&request, &Settings::default(), Network::Internet, "linux").unwrap_err();
        assert!(matches!(error, ProfileError::Exposes { .. }), "{error}");
    }

    #[test]
    fn the_profiles_directory_is_never_readable_by_a_run() {
        let dir = tempfile::tempdir().unwrap();
        let mut request = request();
        request
            .env
            .insert("PATH".into(), dir.path().to_string_lossy().into_owned());
        let settings = Settings {
            profiles_dir: dir.path().to_path_buf(),
            ..Settings::default()
        };
        let error = build(&request, &settings, Network::Internet, "linux").unwrap_err();
        assert!(matches!(error, ProfileError::Exposes { .. }), "{error}");
    }

    #[test]
    fn relative_and_empty_path_entries_are_left_out_not_granted() {
        let mut request = request();
        request
            .env
            .insert("PATH".into(), ".:/nonexistent-bin::".into());
        let profile = build(&request, &Settings::default(), Network::Moebius, "linux").unwrap();
        for path in grants(&profile) {
            assert!(path.starts_with('/'), "{path}");
        }
    }

    #[test]
    fn a_toolchain_on_the_path_brings_rustup_and_cargos_sources_read_only() {
        let dir = tempfile::tempdir().unwrap();
        let cargo = dir.path().join("cargo");
        let rustup = dir.path().join("rustup");
        for sub in [
            "cargo/bin",
            "cargo/registry",
            "cargo/git",
            "rustup/toolchains",
        ] {
            std::fs::create_dir_all(dir.path().join(sub)).unwrap();
        }
        std::fs::write(cargo.join("credentials.toml"), "token").unwrap();
        let mut request = request();
        request
            .env
            .insert("CARGO_HOME".into(), cargo.to_string_lossy().into_owned());
        request
            .env
            .insert("RUSTUP_HOME".into(), rustup.to_string_lossy().into_owned());
        request.env.insert(
            "PATH".into(),
            cargo.join("bin").to_string_lossy().into_owned(),
        );
        let profile = build(&request, &Settings::default(), Network::Moebius, "linux").unwrap();
        let read = strings(&profile, "filesystem", "read");
        for path in [cargo.join("registry"), cargo.join("git"), rustup.clone()] {
            assert!(
                read.contains(&path.to_string_lossy().into_owned()),
                "{path:?}"
            );
        }
        // Cargo's home is not granted whole: it holds the registry token.
        assert!(!read.contains(&cargo.to_string_lossy().into_owned()));
        // Without a toolchain on the PATH nothing is added.
        request.env.insert("PATH".into(), "/nonexistent-bin".into());
        let profile = build(&request, &Settings::default(), Network::Moebius, "linux").unwrap();
        assert!(
            !strings(&profile, "filesystem", "read")
                .contains(&rustup.to_string_lossy().into_owned())
        );
    }

    #[test]
    fn proxy_and_certificate_variables_are_kept() {
        let mut env = request().env;
        for name in [
            "HTTPS_PROXY",
            "NO_PROXY",
            "SSL_CERT_FILE",
            "NODE_EXTRA_CA_CERTS",
            "CARGO_HOME",
            "RUSTUP_HOME",
            "USER",
            "SHELL",
        ] {
            env.insert(name.into(), "x".into());
        }
        let kept = kept_env(&env);
        for name in [
            "HTTPS_PROXY",
            "NO_PROXY",
            "SSL_CERT_FILE",
            "NODE_EXTRA_CA_CERTS",
            "CARGO_HOME",
            "RUSTUP_HOME",
            "USER",
            "SHELL",
        ] {
            assert!(kept.contains_key(name), "{name}");
        }
        assert!(!kept.contains_key("OPENAI_API_KEY"));
    }

    #[test]
    fn linux_says_plainly_that_the_ports_are_open_on_every_host() {
        let request = request();
        let note = network_note(&request, Network::Moebius, "linux").unwrap();
        assert!(
            note.contains("7172") && note.contains("every host"),
            "{note}"
        );
        assert_eq!(network_note(&request, Network::Internet, "linux"), None);
        assert_eq!(network_note(&request, Network::Moebius, "macos"), None);
    }
}
