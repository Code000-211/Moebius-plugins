//! The mounts a person gives a repository's runs on its page in Moebius (the project setting [`MOUNTS`]), checked and
//! turned into `podman run --volume` values by [`volumes`] when a run is launched (see [`crate::container`]).
//!
//! The setting holds one mount per line; an empty line and a line starting with `#` are skipped:
//!
//! - `<host path>:<container path>[:ro|:rw]`, a folder of the host, read only unless `rw`;
//! - `volume:<name>:<container path>[:ro|:rw]`, a named Podman volume kept between runs, read and write unless `ro`.
//!
//! A mount that would give a run more than the folder it names fails the launch, never the mount alone: a host path
//! that is `/`, the home directory, inside a hidden directory of the home (`.ssh`, `.config`, …), Moebius's data
//! directory, or a path that contains or is inside one of the paths the run already has; and a container path that
//! is `/`, under `/proc`, `/sys` or `/dev`, the run's `HOME`, a path the run already has or one around or inside it,
//! a cache's, or another mount's. A host path is mounted with its links resolved, so a link cannot lead elsewhere.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use crate::container::HOME;
use crate::settings::Cache;

/// The key of the project setting that holds a repository's mounts.
pub const MOUNTS: &str = "mounts";

/// Why the repository's mounts cannot be given to the run; the launch then fails.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("line {line} of the repository's mounts ({text:?}) {why}")]
pub struct MountError {
    pub line: usize,
    pub text: String,
    pub why: String,
}

/// Where a mount comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Source {
    Folder(PathBuf),
    Volume(String),
}

/// One line of the setting.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Mount {
    line: usize,
    text: String,
    source: Source,
    target: PathBuf,
    writable: bool,
}

/// What a mount is checked against: the paths the container already mounts at the same paths, Moebius's data
/// directory, the caches and the home directory of the server's user.
pub struct Granted<'a> {
    pub own: &'a [PathBuf],
    pub data: Option<&'a Path>,
    pub caches: &'a [Cache],
    pub home: Option<&'a Path>,
}

/// The `--volume` values of the mounts in `text`, in the order written.
pub fn volumes(text: &str, granted: &Granted) -> Result<Vec<String>, MountError> {
    let mounts = parse(text)?;
    let mut targets: BTreeSet<PathBuf> = BTreeSet::new();
    let mut values = Vec::new();
    for mount in mounts {
        let bad = |why: &str| MountError {
            line: mount.line,
            text: mount.text.clone(),
            why: why.to_string(),
        };
        check_target(&mount.target, granted).map_err(bad)?;
        if targets.iter().any(|other| overlap(other, &mount.target)) {
            return Err(bad(
                "is mounted at, around or inside the container path of another mount",
            ));
        }
        targets.insert(mount.target.clone());
        let target = mount.target.to_string_lossy();
        values.push(match &mount.source {
            Source::Folder(path) => {
                let real = check_folder(path, granted).map_err(bad)?;
                let options = if mount.writable { "z" } else { "ro,z" };
                format!("{}:{target}:{options}", real.to_string_lossy())
            }
            Source::Volume(name) => {
                let options = if mount.writable { "U,z" } else { "ro,U,z" };
                format!("{name}:{target}:{options}")
            }
        });
    }
    Ok(values)
}

fn parse(text: &str) -> Result<Vec<Mount>, MountError> {
    let mut mounts = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let bad = |why: &str| MountError {
            line: index + 1,
            text: line.to_string(),
            why: why.to_string(),
        };
        if line.contains(',') {
            return Err(bad("holds a comma, which a Podman mount cannot name"));
        }
        let mut parts: Vec<&str> = line.split(':').collect();
        let volume = parts.first() == Some(&"volume");
        if volume {
            parts.remove(0);
        }
        let mode = match parts.last() {
            Some(&"ro") if parts.len() == 3 => Some(false),
            Some(&"rw") if parts.len() == 3 => Some(true),
            _ => None,
        };
        if mode.is_some() {
            parts.pop();
        }
        let [source, target] = parts[..] else {
            return Err(bad(if volume {
                "is not volume:<name>:<container path>[:ro|:rw]"
            } else {
                "is not <host path>:<container path>[:ro|:rw]"
            }));
        };
        let source = if volume {
            let named = source.starts_with(|c: char| c.is_ascii_alphanumeric())
                && source
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c));
            if !named {
                return Err(bad(
                    "names a volume that is not letters, digits, _ . and - starting with a letter or digit",
                ));
            }
            Source::Volume(source.to_string())
        } else {
            if !source.starts_with('/') {
                return Err(bad("names a host path that is not absolute"));
            }
            Source::Folder(PathBuf::from(source))
        };
        let target = Path::new(target);
        let plain = target.is_absolute()
            && target
                .components()
                .all(|c| matches!(c, Component::RootDir | Component::Normal(_)));
        if !plain {
            return Err(bad(
                "names a container path that is not absolute, or has . or ..",
            ));
        }
        mounts.push(Mount {
            line: index + 1,
            text: line.to_string(),
            writable: mode.unwrap_or(volume),
            source,
            target: target.to_path_buf(),
        });
    }
    Ok(mounts)
}

/// Whether one path is the other or is inside it.
fn overlap(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

fn check_target(target: &Path, granted: &Granted) -> Result<(), &'static str> {
    if target == Path::new("/") {
        return Err("is mounted at the container's /");
    }
    if ["/proc", "/sys", "/dev"]
        .iter()
        .any(|system| target.starts_with(system))
    {
        return Err("is mounted under /proc, /sys or /dev");
    }
    if target == Path::new(HOME) || Path::new(HOME).starts_with(target) {
        return Err("is mounted at or around the run's HOME; a path inside it is fine");
    }
    if granted.own.iter().any(|own| overlap(own, target)) {
        return Err(
            "is mounted at, around or inside a path the run already has (its worktree, the clone, the harness)",
        );
    }
    if granted
        .caches
        .iter()
        .any(|cache| overlap(Path::new(&cache.path), target))
    {
        return Err("is mounted at, around or inside a cache of the plugin's --cache");
    }
    Ok(())
}

/// The folder with its links resolved, when the run may have it.
fn check_folder(path: &Path, granted: &Granted) -> Result<PathBuf, &'static str> {
    let real = path
        .canonicalize()
        .map_err(|_| "names a host path that does not exist")?;
    if real == Path::new("/") {
        return Err("mounts the host's /");
    }
    if let Some(home) = granted.home {
        let home = home.canonicalize().unwrap_or_else(|_| home.to_path_buf());
        if home.starts_with(&real) {
            return Err("mounts the home directory or a directory around it");
        }
        let hidden = real
            .strip_prefix(&home)
            .ok()
            .and_then(|rest| rest.components().next())
            .is_some_and(|first| first.as_os_str().to_string_lossy().starts_with('.'));
        if hidden {
            return Err(
                "mounts a hidden directory of the home, where keys and tokens are kept (.ssh, .config, …)",
            );
        }
    }
    let real_or = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if granted
        .data
        .is_some_and(|data| overlap(&real_or(data), &real))
    {
        return Err("mounts Moebius's data directory, a directory around it or one inside it");
    }
    if granted.own.iter().any(|own| overlap(&real_or(own), &real)) {
        return Err(
            "mounts a path the run already has (its worktree, the clone, the harness), or one around or inside it",
        );
    }
    Ok(real)
}

// The paths in these tests are Unix paths.
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A host on disk: a home with a hidden directory and a projects folder, and Moebius's data directory with a
    /// worktree and a clone in it.
    struct Host {
        _dir: tempfile::TempDir,
        root: PathBuf,
        own: Vec<PathBuf>,
        caches: Vec<Cache>,
    }

    fn host() -> Host {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        for sub in [
            "home/.ssh",
            "home/projects/fixtures",
            "data/worktrees/run-7",
            "data/repos/shop",
            "srv/assets",
        ] {
            std::fs::create_dir_all(root.join(sub)).unwrap();
        }
        let own = vec![
            root.join("data/worktrees/run-7"),
            root.join("data/repos/shop"),
        ];
        let caches = vec![Cache {
            volume: "moebius-npm".into(),
            path: "/home/moebius/.npm".into(),
        }];
        Host {
            _dir: dir,
            root,
            own,
            caches,
        }
    }

    impl Host {
        fn volumes(&self, text: &str) -> Result<Vec<String>, MountError> {
            let home = self.root.join("home");
            let data = self.root.join("data");
            volumes(
                &text.replace("$ROOT", &self.root.to_string_lossy()),
                &Granted {
                    own: &self.own,
                    data: Some(&data),
                    caches: &self.caches,
                    home: Some(&home),
                },
            )
        }

        fn why(&self, text: &str) -> String {
            self.volumes(text).unwrap_err().why
        }
    }

    #[test]
    fn folders_are_read_only_and_volumes_writable_unless_the_line_says_otherwise() {
        let host = host();
        let root = host.root.to_string_lossy();
        let text = "# shared with the tests\n\
                    $ROOT/home/projects/fixtures:/fixtures\n\
                    \n\
                    $ROOT/srv/assets:/assets:rw\n\
                    volume:shop-cargo:/home/moebius/.cargo\n\
                    volume:shop-data:/data:ro\n";
        assert_eq!(
            host.volumes(text).unwrap(),
            [
                format!("{root}/home/projects/fixtures:/fixtures:ro,z"),
                format!("{root}/srv/assets:/assets:z"),
                "shop-cargo:/home/moebius/.cargo:U,z".into(),
                "shop-data:/data:ro,U,z".into(),
            ]
        );
        assert_eq!(
            host.volumes("  \n# nothing\n").unwrap(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_line_that_is_not_a_mount_is_refused_with_its_number() {
        let host = host();
        let error = host.volumes("\nfixtures:/fixtures").unwrap_err();
        assert_eq!((error.line, error.text.as_str()), (2, "fixtures:/fixtures"));
        assert!(
            error
                .to_string()
                .starts_with("line 2 of the repository's mounts")
        );
        for bad in [
            "$ROOT/srv/assets",
            "$ROOT/srv/assets:/assets:rx",
            "$ROOT/srv/assets:assets",
            "$ROOT/srv/assets:/assets/../etc",
            "$ROOT/srv/assets:/a,b",
            "volume:/x:/cache",
            "volume:-x:/cache",
            "volume:x",
        ] {
            assert!(host.volumes(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_folder_that_gives_more_than_itself_is_refused() {
        let host = host();
        let wants = [
            ("$ROOT/nowhere:/x", "does not exist"),
            ("/:/x", "the host's /"),
            ("$ROOT/home:/x", "home directory"),
            ("$ROOT:/x", "home directory"),
            ("$ROOT/home/.ssh:/x", "hidden directory"),
            ("$ROOT/data:/x", "data directory"),
            ("$ROOT/data/repos:/x", "data directory"),
        ];
        for (text, why) in wants {
            assert!(host.why(text).contains(why), "{text}: {}", host.why(text));
        }
        // A link is followed to where it leads.
        std::os::unix::fs::symlink(host.root.join("home/.ssh"), host.root.join("srv/keys"))
            .unwrap();
        assert!(
            host.why("$ROOT/srv/keys:/keys")
                .contains("hidden directory")
        );
    }

    #[test]
    fn a_folder_the_run_already_has_is_refused_outside_the_data_directory_too() {
        let mut host = host();
        host.own.push(host.root.join("srv/assets"));
        assert!(host.why("$ROOT/srv:/x").contains("already has"));
        assert!(host.why("$ROOT/srv/assets:/x").contains("already has"));
    }

    #[test]
    fn a_container_path_that_would_hide_what_the_run_needs_is_refused() {
        let host = host();
        let root = host.root.to_string_lossy().into_owned();
        let fixtures = "$ROOT/home/projects/fixtures";
        let wants = [
            ("/".to_string(), "container's /"),
            ("/proc/x".into(), "/proc"),
            ("/dev".into(), "/proc"),
            ("/home/moebius".into(), "HOME"),
            ("/home".into(), "HOME"),
            (format!("{root}/data/worktrees/run-7"), "already has"),
            (format!("{root}/data/worktrees/run-7/src"), "already has"),
            (format!("{root}/data"), "already has"),
            ("/home/moebius/.npm".into(), "cache"),
        ];
        for (target, why) in wants {
            let text = format!("{fixtures}:{target}");
            assert!(host.why(&text).contains(why), "{text}: {}", host.why(&text));
        }
        assert!(
            host.why(&format!("{fixtures}:/a\nvolume:v:/a/b"))
                .contains("another mount")
        );
        assert!(host.volumes("volume:v:/home/moebius/.cargo").is_ok());
    }
}
