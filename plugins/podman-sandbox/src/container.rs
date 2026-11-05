//! The container of one run, as the arguments of `podman run`, and the environment the run keeps.
//!
//! The container is removed when it ends (`--rm`), starts nothing that is not in its image (`--pull never`), keeps no
//! log of what the harness writes (`--log-driver none`, the standard output is Pi's protocol), runs as the user of
//! the server (`--userns=keep-id`, so what it writes in the worktree is the user's) without capabilities or a way to
//! gain them, and reaps orphans (`--init`). It sees exactly:
//!
//! - read and write, at the same paths as on the host: the request's `read_write` paths and the working directory
//!   (the worktree, the agent directory, the sessions directory, the run's temporary directory);
//! - read only, at the same paths: the request's `read_only` paths (the lens, the bridge, the agent's extensions and
//!   skills), the clone whose git directory the worktree uses, and the directory of the harness's installation;
//! - its image, its own `/tmp`, a fresh `HOME` ([`HOME`], a tmpfs of the run) and the `caches` setting's volumes;
//! - the network, through pasta: in mode `moebius` no interface but the loopback, on which the ports of the router,
//!   gateway and check addresses are forwarded to the same ports of the host's loopback (`--splice-only`), so
//!   nothing else is reachable, of the host or of any other machine; in mode `internet` the same ports and general
//!   outbound access;
//! - the environment of [`passed_env`], which the program `podman-sandbox run` writes to the container's standard
//!   input before the harness's protocol, as `NAME=value` lines and an empty line, and which [`ENV_SCRIPT`] exports.
//!   So the run token is in neither Podman's arguments, nor its environment, nor its container configuration.
//!
//! Every host path is mounted with `z`, which labels it for containers on a host with SELinux (Fedora's Podman
//! refuses a mount otherwise) and does nothing elsewhere.
//!
//! The image is the repository's own when a person gave it a Dockerfile on its page in Moebius (the project setting
//! [`DOCKERFILE`]): it is named by the Dockerfile's hash ([`built_image`]), so a changed Dockerfile is a new image,
//! and `podman-sandbox run` builds it before the container starts when Podman does not have it yet. A Dockerfile for
//! the whole server (the app setting [`BASE_DOCKERFILE`]) is built the same way and is what a repository's Dockerfile
//! starts from, through the build argument [`BASE_ARG`]; a repository without a Dockerfile of its own runs on it
//! when it has no `--repo-image`. Without any of those, the image is the `--repo-image` of the repository, else
//! `--image`.
//!
//! The directory of the harness's installation comes from the environment, not from the request, so one that
//! contains the worktree, the clone or another path the run writes fails the launch: it would make the data
//! directory, its secrets and the other worktrees readable too.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use moebius_plugin_api::{LaunchRequest, Urls};

use crate::settings::{Network, Settings};

/// The run's `HOME` in the container, a tmpfs owned by the run's user.
pub const HOME: &str = "/home/moebius";

/// The labels by which the plugin knows its containers and their runs.
pub const PLUGIN_LABEL: &str = "moebius.plugin";
pub const RUN_LABEL: &str = "moebius.run";

/// The prefix of a container's name.
pub const NAME_PREFIX: &str = "moebius-run-";

/// The key of the project setting that holds a repository's Dockerfile.
pub const DOCKERFILE: &str = "dockerfile";

/// The key of the app setting that holds the Dockerfile every repository's Dockerfile starts from.
pub const BASE_DOCKERFILE: &str = "base_dockerfile";

/// The build argument that names the base image when a repository's Dockerfile is built on one, for its
/// `ARG MOEBIUS_BASE` and `FROM ${MOEBIUS_BASE}`.
pub const BASE_ARG: &str = "MOEBIUS_BASE";

/// The repository of the images built from Dockerfiles; the tag is the Dockerfile's hash.
pub const BUILT_IMAGE: &str = "localhost/moebius-podman-sandbox";

/// A Dockerfile to build into `image` when Podman does not have it yet. `base` is the image it starts from, given to
/// the build as [`BASE_ARG`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dockerfile {
    pub image: String,
    pub text: String,
    pub base: Option<String>,
}

/// The image a run uses, and the Dockerfiles to build, in order, before it exists; none for an image of the
/// settings, which the checks at start found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub name: String,
    pub builds: Vec<Dockerfile>,
}

/// The name of the image built from `dockerfile`: the first 16 bytes of its SHA-256 in hexadecimal as the tag, so the
/// same text is built once and a changed one is built anew.
pub fn built_image(dockerfile: &str) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(dockerfile.as_bytes());
    let tag: String = hash[..16].iter().map(|b| format!("{b:02x}")).collect();
    format!("{BUILT_IMAGE}:{tag}")
}

/// The name of the image built from `dockerfile` on the image `base`: named by both, so a changed base is a new
/// image for every repository.
pub fn built_image_on(base: &str, dockerfile: &str) -> String {
    built_image(&format!("{base}\n{dockerfile}"))
}

/// What the container starts with `/bin/sh -c`: it reads `NAME=value` lines from the standard input up to an empty
/// one, exports each (`export "$line"` assigns, it never evaluates the value), then becomes the harness, which reads
/// the rest of the standard input. `read` takes one line from a pipe without reading past it.
pub const ENV_SCRIPT: &str =
    r#"while IFS= read -r line; do [ -z "$line" ] && break; export "$line"; done; exec "$@""#;

/// Why the container of a request cannot be described. The launch then fails, which fails the run: a sandbox that
/// cannot be described is never skipped.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ContainerError {
    #[error("the path {0:?} is not absolute; the sandbox only mounts absolute paths")]
    Relative(String),
    #[error("the path {0:?} contains : or , which a Podman mount cannot name")]
    Unmountable(String),
    #[error("cannot read the host and port of the address {0:?}")]
    BadUrl(String),
    #[error(
        "the address {0:?} is not on this machine's loopback; the container reaches the router only through the \
         host's loopback. Serve the router on 127.0.0.1"
    )]
    NotLoopback(String),
    #[error(
        "there is no image for {0}: give the repository a Dockerfile on its page in Moebius, give the server a base \
         Dockerfile on the Settings page, set --image, or --repo-image <repository>=<image> for its repository, or \
         exclude the agent"
    )]
    NoImage(String),
    #[error(
        "cannot find the harness {0:?} on the run's PATH, so its installation cannot be mounted"
    )]
    NoProgram(String),
    #[error(
        "the harness is installed at {0:?}, in a system directory of the host, which would be mounted over the \
         image's own. Install it under /opt or the home directory"
    )]
    SystemInstall(String),
    #[error(
        "the directory {grant:?} (the harness's installation) would be readable by the run, and it contains \
         {inner:?}, so the data directory, its secrets and other runs' worktrees would be readable too. Install \
         the harness elsewhere"
    )]
    Exposes { grant: String, inner: String },
    #[error(
        "the environment variable {0} cannot be given to the container: its name is not a shell name or its value \
         holds a line break"
    )]
    BadVariable(String),
}

/// The names of the environment variables the container keeps; others are dropped, provider keys among them. `PATH`,
/// `HOME`, `USER` and `SHELL` are the image's (and [`HOME`]), and so are its certificates and toolchains.
const KEPT: [&str; 16] = [
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
    "PI_CODING_AGENT_DIR",
];

/// Whether the container keeps the variable: the usual few, `LC_*`, the harness's, and Moebius's own (`MOEBIUS`,
/// `MOEBIUS_*`: the run token, the check URL and the like).
fn kept(name: &str) -> bool {
    KEPT.contains(&name)
        || name.starts_with("LC_")
        || name == "MOEBIUS"
        || name.starts_with("MOEBIUS_")
}

/// The variables of the request that the container gets, through its standard input.
pub fn passed_env(env: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    env.iter()
        .filter(|(name, _)| kept(name))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect()
}

/// What `podman-sandbox run` writes to the container's standard input before anything else: a `NAME=value` line for
/// each of `names` that `env` has, and an empty line. A name that is not a shell name or a value with a line break
/// would not reach the harness as it is, so it is refused.
pub fn env_lines<'a>(
    env: &BTreeMap<String, String>,
    names: impl IntoIterator<Item = &'a str>,
) -> Result<String, ContainerError> {
    let mut text = String::new();
    for name in names {
        let Some(value) = env.get(name) else {
            continue;
        };
        let shell_name = name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !shell_name || value.contains(['\n', '\r', '\0']) {
            return Err(ContainerError::BadVariable(name.to_string()));
        }
        text.push_str(&format!("{name}={value}\n"));
    }
    text.push('\n');
    Ok(text)
}

/// The repository of the run, by its name in Moebius: the request's, or, from a Moebius that does not send it, the
/// name of its clone's directory (`<data_dir>/repos/<name>`). `None` for a chat without a repository.
pub fn repository(request: &LaunchRequest) -> Option<String> {
    if let Some(name) = &request.repository {
        return Some(name.clone());
    }
    let clone = request.clone.as_deref()?;
    Path::new(clone)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

/// The text of the setting `key` of `settings`, when a person wrote something in it.
fn written<'a>(settings: &'a BTreeMap<String, String>, key: &str) -> Option<&'a str> {
    settings
        .get(key)
        .map(String::as_str)
        .filter(|text| !text.trim().is_empty())
}

/// The image of the run, first that applies: built from the repository's Dockerfile (on the server's base Dockerfile
/// when there is one), the `--repo-image` of the repository, built from the server's base Dockerfile, else `--image`.
pub fn image(request: &LaunchRequest, settings: &Settings) -> Result<Image, ContainerError> {
    let base = written(&request.app_settings, BASE_DOCKERFILE).map(|text| Dockerfile {
        image: built_image(text),
        text: text.to_string(),
        base: None,
    });
    if let Some(text) = written(&request.project_settings, DOCKERFILE) {
        let own = match &base {
            Some(base) => Dockerfile {
                image: built_image_on(&base.image, text),
                text: text.to_string(),
                base: Some(base.image.clone()),
            },
            None => Dockerfile {
                image: built_image(text),
                text: text.to_string(),
                base: None,
            },
        };
        return Ok(Image {
            name: own.image.clone(),
            builds: base.into_iter().chain([own]).collect(),
        });
    }
    let repository = repository(request);
    if let Some(name) = repository
        .as_deref()
        .and_then(|name| settings.repo_images.get(name))
    {
        return Ok(Image {
            name: name.clone(),
            builds: Vec::new(),
        });
    }
    if let Some(base) = base {
        return Ok(Image {
            name: base.image.clone(),
            builds: vec![base],
        });
    }
    let name = settings.image_for(repository.as_deref()).ok_or_else(|| {
        ContainerError::NoImage(match &repository {
            Some(repo) => format!("the repository {repo}"),
            None => format!(
                "the agent {}'s chat without a repository",
                request.run.agent
            ),
        })
    })?;
    Ok(Image {
        name: name.to_string(),
        builds: Vec::new(),
    })
}

/// The ports of the three addresses, which must be on the loopback, sorted and without repeats.
pub fn loopback_ports(urls: &Urls) -> Result<Vec<u16>, ContainerError> {
    let mut ports = BTreeSet::new();
    for url in [&urls.router, &urls.gateway, &urls.check] {
        let (host, port) = host_and_port(url)?;
        let loopback = host == "localhost" || host == "::1" || host.starts_with("127.");
        if !loopback {
            return Err(ContainerError::NotLoopback(url.clone()));
        }
        ports.insert(port);
    }
    Ok(ports.into_iter().collect())
}

fn host_and_port(url: &str) -> Result<(String, u16), ContainerError> {
    let bad = || ContainerError::BadUrl(url.to_string());
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

/// The value of `--network`: pasta, with each port forwarded from the container's loopback to the host's. In mode
/// `moebius` pasta makes no interface but the loopback (`--splice-only`), so those ports are all there is.
pub fn network(mode: Network, ports: &[u16]) -> String {
    let mut options = Vec::new();
    if mode == Network::Moebius {
        options.push("--splice-only".to_string());
    }
    for port in ports {
        options.push("-T".into());
        options.push(port.to_string());
    }
    format!("pasta:{}", options.join(","))
}

/// The system directories of the host, which would be mounted over the image's own: a harness installed there cannot
/// be given to the container.
const SYSTEM_DIRS: [&str; 6] = ["/usr", "/bin", "/sbin", "/lib", "/lib64", "/etc"];

/// The harness's program as the container runs it (the file itself, links resolved) and what of its installation to
/// mount: for a program in a `node_modules` directory its npm package whole (the image then needs `node`); for one in
/// a `bin` or `sbin` directory the file alone, since the directory holds other programs; else its directory, a
/// self-contained installation with the files it reads beside it.
pub fn program_mount(program: &str, path_var: Option<&str>) -> Option<(PathBuf, PathBuf)> {
    let found = if program.contains('/') {
        PathBuf::from(program)
    } else {
        std::env::split_paths(path_var?)
            .map(|dir| dir.join(program))
            .find(|candidate| candidate.is_file())?
    };
    let real = found.canonicalize().ok()?;
    let components: Vec<_> = real.components().collect();
    let package = components
        .iter()
        .rposition(|c| c.as_os_str() == "node_modules")
        .filter(|index| index + 1 < components.len());
    let dir = match package {
        Some(index) => {
            let scoped = components[index + 1]
                .as_os_str()
                .to_string_lossy()
                .starts_with('@');
            let end = (index + if scoped { 3 } else { 2 }).min(components.len() - 1);
            components[..end].iter().collect()
        }
        None => {
            let parent = real.parent()?;
            if parent
                .file_name()
                .is_some_and(|name| name == "bin" || name == "sbin")
            {
                real.clone()
            } else {
                parent.to_path_buf()
            }
        }
    };
    Some((real, dir))
}

/// Whether `inner` is inside `grant` or is it, compared as written and, where the paths exist, with links resolved.
fn contains(grant: &Path, inner: &Path) -> bool {
    if inner.starts_with(grant) {
        return true;
    }
    match (grant.canonicalize(), inner.canonicalize()) {
        (Ok(grant), Ok(inner)) => inner.starts_with(grant),
        _ => false,
    }
}

fn mountable(path: &Path) -> Result<String, ContainerError> {
    let text = path.to_string_lossy().into_owned();
    if !path.is_absolute() {
        return Err(ContainerError::Relative(text));
    }
    if text.contains([':', ',']) {
        return Err(ContainerError::Unmountable(text));
    }
    Ok(text)
}

/// The arguments after `podman` that start the run of `request` in the container `name`, with the network mode
/// `mode`, and the image they use.
pub fn build(
    request: &LaunchRequest,
    settings: &Settings,
    mode: Network,
    name: &str,
) -> Result<(Vec<String>, Image), ContainerError> {
    let image = image(request, settings)?;
    let ports = loopback_ports(&request.urls)?;

    let path_var = request.env.get("PATH").map(String::as_str);
    let (program, install) = program_mount(&request.program, path_var)
        .ok_or_else(|| ContainerError::NoProgram(request.program.clone()))?;
    if SYSTEM_DIRS.iter().any(|dir| install.starts_with(dir)) {
        return Err(ContainerError::SystemInstall(
            install.to_string_lossy().into_owned(),
        ));
    }
    let own: Vec<PathBuf> = request
        .read_write
        .iter()
        .chain([&request.dir])
        .chain(&request.read_only)
        .chain(&request.clone)
        .map(PathBuf::from)
        .collect();
    if !own.contains(&install) {
        let protected = request
            .read_write
            .iter()
            .chain([&request.dir])
            .chain(&request.clone)
            .map(PathBuf::from);
        for inner in protected {
            if contains(&install, &inner) {
                return Err(ContainerError::Exposes {
                    grant: install.to_string_lossy().into_owned(),
                    inner: inner.to_string_lossy().into_owned(),
                });
            }
        }
    }

    // Writable wins over read only for a path named both ways; parents come before what is inside them.
    let mut mounts: BTreeMap<String, bool> = BTreeMap::new();
    for path in request.read_write.iter().chain([&request.dir]) {
        mounts.insert(mountable(Path::new(path))?, true);
    }
    let read_only = request
        .read_only
        .iter()
        .chain(&request.clone)
        .map(PathBuf::from)
        .chain([install]);
    for path in read_only {
        mounts.entry(mountable(&path)?).or_insert(false);
    }

    let mut args: Vec<String> = [
        "run",
        "--rm",
        "--interactive",
        "--init",
        "--pull",
        "never",
        "--log-driver",
        "none",
        "--name",
        name,
        "--label",
        &format!("{PLUGIN_LABEL}={}", crate::NAME),
        "--label",
        &format!("{RUN_LABEL}={}", request.run.id),
        "--userns",
        "keep-id",
        "--cap-drop",
        "ALL",
        "--security-opt",
        "no-new-privileges",
        "--network",
        &network(mode, &ports),
        "--tmpfs",
        &format!("{HOME}:rw,exec,mode=0700,U"),
        "--env",
        &format!("HOME={HOME}"),
    ]
    .map(String::from)
    .into();
    for cache in &settings.caches {
        args.push("--volume".into());
        args.push(format!("{}:{}:U,z", cache.volume, cache.path));
    }
    for (path, writable) in &mounts {
        let options = if *writable { "z" } else { "ro,z" };
        args.push("--volume".into());
        args.push(format!("{path}:{path}:{options}"));
    }
    args.extend([
        "--workdir".to_string(),
        mountable(Path::new(&request.dir))?,
        image.name.clone(),
        "/bin/sh".into(),
        "-c".into(),
        ENV_SCRIPT.into(),
        "moebius-env".into(),
        program.to_string_lossy().into_owned(),
    ]);
    args.extend(request.args.iter().cloned());
    Ok((args, image))
}

// The paths in these tests are Unix paths.
#[cfg(all(test, unix))]
mod tests {
    use moebius_plugin_api::LaunchRun;

    use super::*;
    use crate::settings::Cache;

    /// A world on disk: a harness program and the run's directories, which `program_mount` and the exposure check
    /// look at.
    struct World {
        _dir: tempfile::TempDir,
        root: PathBuf,
    }

    fn world() -> World {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        for sub in [
            "bin",
            "data/worktrees/run-7",
            "data/runs/7/tmp",
            "data/repos/shop",
        ] {
            std::fs::create_dir_all(root.join(sub)).unwrap();
        }
        std::fs::write(root.join("bin/pi-bolt"), "").unwrap();
        World { _dir: dir, root }
    }

    fn request(world: &World) -> LaunchRequest {
        let at = |p: &str| world.root.join(p).to_string_lossy().into_owned();
        LaunchRequest {
            run: LaunchRun {
                id: 7,
                purpose: "code".into(),
                agent: "Coder".into(),
                chat: false,
                card: Some(3),
            },
            program: "pi-bolt".into(),
            args: vec!["--mode".into(), "rpc".into()],
            env: BTreeMap::from([
                ("PATH".to_string(), at("bin")),
                ("HOME".to_string(), "/home/someone".to_string()),
                (
                    "MOEBIUS_RUN_TOKEN".to_string(),
                    "moebius-run-secret".to_string(),
                ),
                ("OPENAI_API_KEY".to_string(), "sk-provider".to_string()),
                ("LC_ALL".to_string(), "C.UTF-8".to_string()),
            ]),
            dir: at("data/worktrees/run-7"),
            read_only: vec![at("data/runs/7/bridge.ts")],
            read_write: vec![at("data/worktrees/run-7"), at("data/runs/7/tmp")],
            urls: Urls {
                router: "http://127.0.0.1:7172".into(),
                gateway: "http://127.0.0.1:7172/mcp".into(),
                check: "http://127.0.0.1:7173".into(),
            },
            clone: Some(at("data/repos/shop")),
            runner: None,
            repository: None,
            project_settings: BTreeMap::new(),
            app_settings: BTreeMap::new(),
        }
    }

    fn settings() -> Settings {
        Settings {
            image: Some("localhost/base".into()),
            ..Settings::default()
        }
    }

    fn after<'a>(args: &'a [String], flag: &str) -> Vec<&'a str> {
        args.windows(2)
            .filter(|pair| pair[0] == flag)
            .map(|pair| pair[1].as_str())
            .collect()
    }

    #[test]
    fn the_run_mounts_what_it_writes_and_reads_at_the_same_paths() {
        let world = world();
        let request = request(&world);
        let (args, image) =
            build(&request, &settings(), Network::Moebius, "moebius-run-7-a").unwrap();
        assert_eq!(image.name, "localhost/base");
        let at = |p: &str| world.root.join(p).to_string_lossy().into_owned();
        let volumes = after(&args, "--volume");
        for (path, options) in [
            (at("data/worktrees/run-7"), "z"),
            (at("data/runs/7/tmp"), "z"),
            (at("data/runs/7/bridge.ts"), "ro,z"),
            (at("data/repos/shop"), "ro,z"),
            (at("bin/pi-bolt"), "ro,z"),
        ] {
            assert!(
                volumes.contains(&format!("{path}:{path}:{options}").as_str()),
                "{path} in {volumes:?}"
            );
        }
        assert_eq!(
            volumes.len(),
            5,
            "the worktree, named twice, is mounted once"
        );
        assert_eq!(after(&args, "--workdir"), [at("data/worktrees/run-7")]);
        let tail = &args[args.iter().position(|a| a == "localhost/base").unwrap()..];
        assert_eq!(
            tail,
            [
                "localhost/base",
                "/bin/sh",
                "-c",
                ENV_SCRIPT,
                "moebius-env",
                &at("bin/pi-bolt"),
                "--mode",
                "rpc"
            ]
        );
    }

    #[test]
    fn nothing_else_of_the_host_is_mounted() {
        let world = world();
        let (args, _) = build(&request(&world), &settings(), Network::Moebius, "n").unwrap();
        let data = world.root.join("data").to_string_lossy().into_owned();
        for volume in after(&args, "--volume") {
            let source = volume.split(':').next().unwrap();
            assert_ne!(source, data, "the data directory itself is never mounted");
            assert!(!source.ends_with("secrets.toml") && !source.ends_with("moebius.toml"));
        }
        assert!(
            !args.iter().any(|a| a.contains("/home/someone")),
            "{args:?}"
        );
    }

    #[test]
    fn the_container_is_rootless_without_capabilities_and_leaves_nothing() {
        let world = world();
        let (args, _) = build(
            &request(&world),
            &settings(),
            Network::Moebius,
            "moebius-run-7-a",
        )
        .unwrap();
        for flag in ["--rm", "--interactive", "--init"] {
            assert!(args.contains(&flag.to_string()), "{flag}");
        }
        assert_eq!(after(&args, "--pull"), ["never"]);
        assert_eq!(after(&args, "--log-driver"), ["none"]);
        assert_eq!(after(&args, "--userns"), ["keep-id"]);
        assert_eq!(after(&args, "--cap-drop"), ["ALL"]);
        assert_eq!(after(&args, "--security-opt"), ["no-new-privileges"]);
        assert_eq!(after(&args, "--name"), ["moebius-run-7-a"]);
        assert_eq!(
            after(&args, "--label"),
            ["moebius.plugin=podman-sandbox", "moebius.run=7"]
        );
        assert_eq!(
            after(&args, "--tmpfs"),
            ["/home/moebius:rw,exec,mode=0700,U"]
        );
        assert_eq!(after(&args, "--env"), ["HOME=/home/moebius"]);
    }

    #[test]
    fn mode_moebius_forwards_only_the_ports_and_internet_adds_the_rest() {
        assert_eq!(
            network(Network::Moebius, &[7172, 7173]),
            "pasta:--splice-only,-T,7172,-T,7173"
        );
        assert_eq!(network(Network::Internet, &[7172]), "pasta:-T,7172");
        let world = world();
        let (args, _) = build(&request(&world), &settings(), Network::Moebius, "n").unwrap();
        assert_eq!(
            after(&args, "--network"),
            ["pasta:--splice-only,-T,7172,-T,7173"]
        );
    }

    #[test]
    fn an_address_off_the_loopback_is_refused() {
        let world = world();
        let mut request = request(&world);
        request.urls.check = "http://10.0.0.5:7173".into();
        assert_eq!(
            build(&request, &settings(), Network::Internet, "n"),
            Err(ContainerError::NotLoopback("http://10.0.0.5:7173".into()))
        );
        let urls = |router: &str| Urls {
            router: router.into(),
            gateway: "http://localhost:7172/mcp".into(),
            check: "http://[::1]:7172".into(),
        };
        assert_eq!(
            loopback_ports(&urls("https://127.0.0.1")),
            Ok(vec![443, 7172])
        );
        assert!(matches!(
            loopback_ports(&urls("http://user@127.0.0.1:1")),
            Err(ContainerError::BadUrl(_))
        ));
        assert!(matches!(
            loopback_ports(&urls("ftp://127.0.0.1:1")),
            Err(ContainerError::BadUrl(_))
        ));
    }

    #[test]
    fn the_image_is_the_repositorys_else_the_default_else_none() {
        let world = world();
        let mut settings = settings();
        settings
            .repo_images
            .insert("shop".into(), "localhost/shop".into());
        let (_, image) = build(&request(&world), &settings, Network::Moebius, "n").unwrap();
        assert_eq!(image.name, "localhost/shop");
        assert!(image.builds.is_empty());

        let mut chat = request(&world);
        chat.clone = None;
        let (_, image) = build(&chat, &settings, Network::Moebius, "n").unwrap();
        assert_eq!(image.name, "localhost/base");

        settings.image = None;
        let error = build(&chat, &settings, Network::Moebius, "n").unwrap_err();
        assert!(
            error.to_string().contains("Coder") && error.to_string().contains("--image"),
            "{error}"
        );
        let mut other = request(&world);
        other.clone = Some(world.root.join("data/repos/other").to_string_lossy().into());
        let error = build(&other, &settings, Network::Moebius, "n").unwrap_err();
        assert!(error.to_string().contains("repository other"), "{error}");
    }

    #[test]
    fn a_repositorys_dockerfile_names_its_own_image_by_its_hash_before_any_setting() {
        let world = world();
        let mut settings = settings();
        settings
            .repo_images
            .insert("shop".into(), "localhost/shop".into());
        let mut request = request(&world);
        request.repository = Some("shop".into());
        request
            .project_settings
            .insert(DOCKERFILE.into(), "FROM debian\n".into());
        let (args, image) = build(&request, &settings, Network::Moebius, "n").unwrap();
        assert!(image.name.starts_with("localhost/moebius-podman-sandbox:"));
        assert_eq!(image.name.len(), BUILT_IMAGE.len() + 1 + 32);
        assert_eq!(image.builds.len(), 1);
        assert_eq!(image.builds[0].text, "FROM debian\n");
        assert_eq!(image.builds[0].base, None);
        assert!(args.contains(&image.name));

        // The same text is the same image; another text another one.
        assert_eq!(built_image("FROM debian\n"), image.name);
        assert_ne!(built_image("FROM debian:12\n"), image.name);

        // A blank Dockerfile is none: the settings' image is used.
        request
            .project_settings
            .insert(DOCKERFILE.into(), " \n".into());
        let (_, image) = build(&request, &settings, Network::Moebius, "n").unwrap();
        assert_eq!(image.name, "localhost/shop");
        assert!(image.builds.is_empty());
    }

    #[test]
    fn the_servers_base_dockerfile_is_built_first_and_a_repositorys_dockerfile_is_built_on_it() {
        let world = world();
        let settings = settings();
        let mut request = request(&world);
        request
            .app_settings
            .insert(BASE_DOCKERFILE.into(), "FROM debian\n".into());
        request
            .project_settings
            .insert(DOCKERFILE.into(), "FROM ${MOEBIUS_BASE}\n".into());
        let (args, image) = build(&request, &settings, Network::Moebius, "n").unwrap();
        let base = built_image("FROM debian\n");
        assert_eq!(image.builds.len(), 2);
        assert_eq!(image.builds[0].image, base);
        assert_eq!(image.builds[0].base, None);
        assert_eq!(image.builds[1].base.as_deref(), Some(base.as_str()));
        // The run is of the last one, which is named by the base too: another base is another image.
        assert_eq!(image.name, image.builds[1].image);
        assert_eq!(image.name, built_image_on(&base, "FROM ${MOEBIUS_BASE}\n"));
        assert_ne!(
            image.name,
            built_image_on(&built_image("FROM debian:12\n"), "FROM ${MOEBIUS_BASE}\n")
        );
        assert!(args.contains(&image.name));

        // Without a base the repository's Dockerfile is built alone, as it always was.
        request.app_settings.clear();
        let (_, image) = build(&request, &settings, Network::Moebius, "n").unwrap();
        assert_eq!(image.name, built_image("FROM ${MOEBIUS_BASE}\n"));
        assert_eq!(image.builds.len(), 1);
        assert_eq!(image.builds[0].base, None);
    }

    #[test]
    fn a_repository_without_a_dockerfile_runs_on_the_base_after_its_own_image_and_before_the_default()
     {
        let world = world();
        let mut settings = settings();
        let mut request = request(&world);
        request
            .app_settings
            .insert(BASE_DOCKERFILE.into(), "FROM debian\n".into());

        // The default image is only for what has nothing else.
        let (args, image) = build(&request, &settings, Network::Moebius, "n").unwrap();
        assert_eq!(image.name, built_image("FROM debian\n"));
        assert_eq!(image.builds.len(), 1);
        assert!(args.contains(&image.name));

        // The image named for the repository is a decision about that repository.
        settings
            .repo_images
            .insert("shop".into(), "localhost/shop".into());
        let (_, image) = build(&request, &settings, Network::Moebius, "n").unwrap();
        assert_eq!(image.name, "localhost/shop");
        assert!(image.builds.is_empty());

        // A chat without a repository runs on the base too, and a blank base is none.
        settings.repo_images.clear();
        request.clone = None;
        let (_, image) = build(&request, &settings, Network::Moebius, "n").unwrap();
        assert_eq!(image.name, built_image("FROM debian\n"));
        request
            .app_settings
            .insert(BASE_DOCKERFILE.into(), "  \n".into());
        let (_, image) = build(&request, &settings, Network::Moebius, "n").unwrap();
        assert_eq!(image.name, "localhost/base");
    }

    #[test]
    fn the_repository_is_the_one_the_request_names_before_the_clone_directory() {
        let world = world();
        let mut settings = settings();
        settings
            .repo_images
            .insert("renamed".into(), "localhost/renamed".into());
        let mut request = request(&world);
        assert_eq!(repository(&request).as_deref(), Some("shop"));
        request.repository = Some("renamed".into());
        let (_, image) = build(&request, &settings, Network::Moebius, "n").unwrap();
        assert_eq!(image.name, "localhost/renamed");
    }

    #[test]
    fn caches_are_named_volumes_owned_by_the_run_user() {
        let world = world();
        let mut settings = settings();
        settings.caches.push(Cache {
            volume: "moebius-npm".into(),
            path: "/home/moebius/.npm".into(),
        });
        let (args, _) = build(&request(&world), &settings, Network::Moebius, "n").unwrap();
        assert!(after(&args, "--volume").contains(&"moebius-npm:/home/moebius/.npm:U,z"));
    }

    #[test]
    fn the_harness_is_run_by_its_real_path_from_its_installation() {
        let world = world();
        let pkg = world.root.join("lib/node_modules/@acme/pi-bolt/dist");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(pkg.join("cli.js"), "").unwrap();
        std::os::unix::fs::symlink(pkg.join("cli.js"), world.root.join("bin/pi")).unwrap();
        let path = world.root.join("bin").to_string_lossy().into_owned();
        let (program, dir) = program_mount("pi", Some(&path)).unwrap();
        assert_eq!(program, pkg.join("cli.js"));
        assert_eq!(dir, world.root.join("lib/node_modules/@acme/pi-bolt"));
        // A program of a bin directory is mounted alone, not with the other programs there.
        let (program, dir) = program_mount("pi-bolt", Some(&path)).unwrap();
        assert_eq!(
            (program, dir),
            (
                world.root.join("bin/pi-bolt"),
                world.root.join("bin/pi-bolt")
            )
        );
        // A self-contained installation, linked from a bin directory, is mounted whole.
        let install = world.root.join("pi-bolt-linux-x64");
        std::fs::create_dir_all(install.join("assets")).unwrap();
        std::fs::write(install.join("pi"), "").unwrap();
        std::os::unix::fs::symlink(install.join("pi"), world.root.join("bin/pb")).unwrap();
        let (program, dir) = program_mount("pb", Some(&path)).unwrap();
        assert_eq!((program, dir), (install.join("pi"), install));
        assert_eq!(program_mount("absent", Some(&path)), None);

        let mut request = request(&world);
        request.program = "absent".into();
        assert_eq!(
            build(&request, &settings(), Network::Moebius, "n"),
            Err(ContainerError::NoProgram("absent".into()))
        );
    }

    #[test]
    fn a_harness_installed_in_a_system_directory_fails_the_launch() {
        let world = world();
        let mut request = request(&world);
        request.program = "/bin/sh".into();
        let error = build(&request, &settings(), Network::Moebius, "n").unwrap_err();
        assert!(matches!(error, ContainerError::SystemInstall(_)), "{error}");
    }

    #[test]
    fn a_harness_installed_around_the_data_directory_fails_the_launch() {
        let world = world();
        std::fs::write(world.root.join("pi-bolt"), "").unwrap();
        let mut request = request(&world);
        request.program = world.root.join("pi-bolt").to_string_lossy().into();
        let error = build(&request, &settings(), Network::Moebius, "n").unwrap_err();
        assert!(matches!(error, ContainerError::Exposes { .. }), "{error}");
    }

    #[test]
    fn paths_a_mount_cannot_name_are_refused() {
        let world = world();
        let mut request = request(&world);
        request.read_only.push("/data/a:b".into());
        assert_eq!(
            build(&request, &settings(), Network::Moebius, "n"),
            Err(ContainerError::Unmountable("/data/a:b".into()))
        );
        request.read_only = vec!["relative".into()];
        assert_eq!(
            build(&request, &settings(), Network::Moebius, "n"),
            Err(ContainerError::Relative("relative".into()))
        );
    }

    #[test]
    fn the_container_keeps_the_runs_variables_and_no_provider_keys() {
        let world = world();
        let env = passed_env(&request(&world).env);
        assert_eq!(
            env.keys().map(String::as_str).collect::<Vec<_>>(),
            ["LC_ALL", "MOEBIUS_RUN_TOKEN"]
        );
    }

    #[test]
    fn the_run_token_is_never_an_argument() {
        let world = world();
        let (args, _) = build(&request(&world), &settings(), Network::Moebius, "n").unwrap();
        let all = args.join(" ");
        assert!(
            !all.contains("moebius-run-secret") && !all.contains("sk-provider"),
            "{all}"
        );
    }

    #[test]
    fn env_lines_are_name_value_lines_and_an_empty_one() {
        let env = BTreeMap::from([
            ("A".to_string(), "x y $z `w` \\ \"q\" 'r'".to_string()),
            ("B_2".to_string(), String::new()),
        ]);
        assert_eq!(
            env_lines(&env, ["A", "B_2", "MISSING"]).unwrap(),
            "A=x y $z `w` \\ \"q\" 'r'\nB_2=\n\n"
        );
        assert_eq!(env_lines(&env, []).unwrap(), "\n");
        let broken = BTreeMap::from([("A".to_string(), "one\ntwo".to_string())]);
        assert_eq!(
            env_lines(&broken, ["A"]),
            Err(ContainerError::BadVariable("A".into()))
        );
        let named = BTreeMap::from([("1A".to_string(), "x".to_string())]);
        assert!(env_lines(&named, ["1A"]).is_err());
    }
}
