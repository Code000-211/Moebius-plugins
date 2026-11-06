//! The checks made once when the plugin starts: the platform is Linux, the engine is there and makes containers that
//! are the server's user's, and every image the settings name is in its storage. Podman must be rootless, 4.4 or later,
//! with pasta; Docker must be 20.10 or later, a daemon on this machine (not Docker Desktop's virtual machine), and
//! neither rootless nor remapping user namespaces, since either would make what a run writes another uid's. Settings
//! may name no image, since each repository can bring its own Dockerfile; a run that ends up with none is refused at
//! its launch. A plugin that fails them exits with the sentence, so the core marks it not running and every launch is
//! refused: a run never starts outside a container because the container cannot be made.

use std::process::Command;

use serde_json::Value;

use crate::settings::{Engine, Settings};

/// The oldest Podman whose `--network pasta:<options>` passes the options through to pasta.
pub const MINIMUM_PODMAN: (u32, u32) = (4, 4);

/// The oldest Docker whose `run` takes `--pull never`.
pub const MINIMUM_DOCKER: (u32, u32) = (20, 10);

/// Why the plugin cannot start.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CheckError {
    #[error(
        "this platform ({0}) is not one this plugin runs on: only Linux, where the containers run without a \
         virtual machine"
    )]
    Platform(String),
    #[error(
        "cannot run {program:?}: {why}. Install {engine} or set --engine-program to its path",
        engine = engine.title()
    )]
    NotFound {
        engine: Engine,
        program: String,
        why: String,
    },
    #[error("`{program} info` failed or did not print JSON: {said}")]
    NoInfo { program: String, said: String },
    #[error(
        "Podman runs as root here; this plugin needs rootless Podman, so that a run's container is the user's and \
         not root's"
    )]
    Rootful,
    #[error(
        "Docker runs rootless or remaps user namespaces here, so what a run writes would belong to another uid than \
         the server's user. Use a Docker daemon without either, or --engine podman"
    )]
    RemappedDocker,
    #[error(
        "Docker is Docker Desktop here, whose containers run in a virtual machine that does not share the host's \
         paths and sockets as this plugin needs. Use Docker Engine, or --engine podman"
    )]
    DockerDesktop,
    #[error(
        "{engine} {found} is older than {}.{}, {why}",
        minimum.0,
        minimum.1,
        engine = engine.title(),
        why = match engine {
            Engine::Podman => "the first that passes pasta's options through",
            Engine::Docker => "the first whose run takes --pull never",
        }
    )]
    Version {
        engine: Engine,
        found: String,
        minimum: (u32, u32),
    },
    #[error(
        "Podman has no pasta, which makes the container's network (only the router's ports in mode \"moebius\"). \
         Install pasta (the package passt)"
    )]
    NoPasta,
    #[error(
        "the image {image:?} is not in {engine}'s storage; the plugin never pulls one. Build or pull it (`{} pull`), \
         then restart the server",
        engine.name(),
        engine = engine.title()
    )]
    MissingImage { engine: Engine, image: String },
}

/// Checks the platform, the engine and the images. `os` is `std::env::consts::OS`.
pub fn check(settings: &Settings, os: &str) -> Result<(), CheckError> {
    if os != "linux" {
        return Err(CheckError::Platform(os.to_string()));
    }
    // Docker's `info` has no `json` format before 23.0; a template does the same on every version.
    let format = match settings.engine {
        Engine::Podman => "json",
        Engine::Docker => "{{json .}}",
    };
    let output = engine(settings, &["info", "--format", format])?;
    let info: Value = output
        .success
        .then(|| serde_json::from_str(&output.stdout).ok())
        .flatten()
        .ok_or_else(|| CheckError::NoInfo {
            program: settings.program.clone(),
            said: last_line(&output.stderr),
        })?;
    match settings.engine {
        Engine::Podman => check_podman(&info)?,
        Engine::Docker => check_docker(&info)?,
    }
    for image in settings.images() {
        if !engine(settings, &image_exists(settings.engine, image))?.success {
            return Err(CheckError::MissingImage {
                engine: settings.engine,
                image: image.to_string(),
            });
        }
    }
    Ok(())
}

fn check_podman(info: &Value) -> Result<(), CheckError> {
    if info["host"]["security"]["rootless"] != Value::Bool(true) {
        return Err(CheckError::Rootful);
    }
    let found = info["version"]["Version"].as_str().unwrap_or("");
    at_least(Engine::Podman, found, MINIMUM_PODMAN)?;
    // Podman 5 says where its pasta is; Podman 4 finds it on the PATH.
    let pasta = info["host"]["pasta"]["executable"]
        .as_str()
        .is_some_and(|path| !path.is_empty())
        || resolve("pasta").is_some();
    if !pasta {
        return Err(CheckError::NoPasta);
    }
    Ok(())
}

fn check_docker(info: &Value) -> Result<(), CheckError> {
    // Each option is a line such as `name=seccomp,profile=builtin`.
    let remapped = info["SecurityOptions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(|option| option.split(',').next())
        .any(|name| name == "name=rootless" || name == "name=userns");
    if remapped {
        return Err(CheckError::RemappedDocker);
    }
    if info["OperatingSystem"]
        .as_str()
        .is_some_and(|os| os.contains("Docker Desktop"))
    {
        return Err(CheckError::DockerDesktop);
    }
    at_least(
        Engine::Docker,
        info["ServerVersion"].as_str().unwrap_or(""),
        MINIMUM_DOCKER,
    )
}

fn at_least(engine: Engine, found: &str, minimum: (u32, u32)) -> Result<(), CheckError> {
    if version(found).is_none_or(|version| version < minimum) {
        return Err(CheckError::Version {
            engine,
            found: found.to_string(),
            minimum,
        });
    }
    Ok(())
}

/// The arguments after the engine's program that succeed when it has `image`: Docker has no `image exists`.
pub fn image_exists(engine: Engine, image: &str) -> Vec<String> {
    match engine {
        Engine::Podman => vec!["image".into(), "exists".into(), image.into()],
        Engine::Docker => vec![
            "image".into(),
            "inspect".into(),
            "--format".into(),
            "{{.Id}}".into(),
            image.into(),
        ],
    }
}

/// The major and minor version of a version such as `5.8.7`, `4.9.3-dev` or `27.3.1`.
fn version(text: &str) -> Option<(u32, u32)> {
    let mut parts = text.split(['.', '-']);
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

fn last_line(text: &str) -> String {
    text.lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("nothing")
        .trim()
        .to_string()
}

/// The absolute path of `program`: itself when it has a directory part, else the first `PATH` entry that holds it.
pub fn resolve(program: &str) -> Option<String> {
    let path = std::path::Path::new(program);
    let found = if path.components().count() > 1 {
        path.to_path_buf()
    } else {
        std::env::split_paths(&std::env::var_os("PATH")?)
            .map(|dir| dir.join(program))
            .find(|candidate| candidate.is_file())?
    };
    std::path::absolute(found)
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

struct Ran {
    success: bool,
    stdout: String,
    stderr: String,
}

fn engine(settings: &Settings, args: &[impl AsRef<std::ffi::OsStr>]) -> Result<Ran, CheckError> {
    let output = Command::new(&settings.program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| CheckError::NotFound {
            engine: settings.engine,
            program: settings.program.clone(),
            why: e.to_string(),
        })?;
    Ok(Ran {
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_read_by_major_and_minor() {
        assert_eq!(version("5.8.7"), Some((5, 8)));
        assert_eq!(version("4.9.3-dev"), Some((4, 9)));
        assert_eq!(version("4"), None);
        assert_eq!(version(""), None);
        assert!(version("4.3.1").unwrap() < MINIMUM_PODMAN);
        assert!(version("4.10.0").unwrap() >= MINIMUM_PODMAN);
        assert_eq!(version("27.3.1"), Some((27, 3)));
        assert!(version("20.10.24").unwrap() >= MINIMUM_DOCKER);
        assert!(version("19.03.15").unwrap() < MINIMUM_DOCKER);
    }

    #[test]
    fn docker_has_no_image_exists_so_it_is_asked_to_inspect() {
        assert_eq!(
            image_exists(Engine::Podman, "base"),
            ["image", "exists", "base"]
        );
        assert_eq!(
            image_exists(Engine::Docker, "base"),
            ["image", "inspect", "--format", "{{.Id}}", "base"]
        );
    }

    #[test]
    fn other_platforms_are_refused_before_the_engine_is_run() {
        let settings = Settings {
            program: "/does/not/exist".into(),
            image: Some("base".into()),
            ..Settings::default()
        };
        assert_eq!(
            check(&settings, "macos"),
            Err(CheckError::Platform("macos".into()))
        );
        // No image is fine: the repositories may bring Dockerfiles. The engine is still checked.
        let imageless = Settings {
            image: None,
            ..settings.clone()
        };
        assert!(matches!(
            check(&imageless, "linux"),
            Err(CheckError::NotFound { .. })
        ));
        let error = check(&settings, "linux").unwrap_err();
        assert!(matches!(error, CheckError::NotFound { .. }));
        assert!(
            error
                .to_string()
                .contains("Install Podman or set --engine-program")
        );
        let docker = Settings {
            engine: Engine::Docker,
            ..settings
        };
        assert!(
            check(&docker, "linux")
                .unwrap_err()
                .to_string()
                .contains("Install Docker")
        );
    }
}

// The fake engines are shell scripts.
#[cfg(all(test, unix))]
mod engine_tests {
    use std::path::Path;

    use super::*;

    /// A fake engine whose `info` prints `info` and whose `image exists` (or `image inspect`) succeeds for the images
    /// in `images`, named by its last argument.
    fn fake_engine(dir: &Path, engine: Engine, info: &str, images: &[&str]) -> Settings {
        let path = dir.join(engine.name());
        let known = images.join(" ");
        let script = format!(
            "#!/bin/sh\nfor last; do :; done\ncase \"$1\" in\n  info) cat <<'EOF'\n{info}\nEOF\n;;\n  image) for i in {known}; do [ \"$last\" = \"$i\" ] && exit 0; done; exit 1 ;;\nesac\n"
        );
        crate::testing::script(&path, &script);
        Settings {
            engine,
            program: path.to_string_lossy().into_owned(),
            image: Some("localhost/base".into()),
            ..Settings::default()
        }
    }

    fn fake(dir: &Path, info: &str, images: &[&str]) -> Settings {
        fake_engine(dir, Engine::Podman, info, images)
    }

    fn docker_info(version: &str, os: &str, options: &[&str]) -> String {
        serde_json::json!({
            "ServerVersion": version,
            "OperatingSystem": os,
            "SecurityOptions": options,
        })
        .to_string()
    }

    #[test]
    fn a_local_docker_that_runs_as_root_with_the_images_passes() {
        let dir = tempfile::tempdir().unwrap();
        let info = docker_info(
            "27.3.1",
            "Ubuntu 24.04.1 LTS",
            &["name=seccomp,profile=builtin", "name=cgroupns"],
        );
        let settings = fake_engine(dir.path(), Engine::Docker, &info, &["localhost/base"]);
        assert_eq!(check(&settings, "linux"), Ok(()));
        let missing = Settings {
            image: Some("localhost/shop".into()),
            ..settings
        };
        let error = check(&missing, "linux").unwrap_err();
        assert!(
            error.to_string().contains("Docker's storage")
                && error.to_string().contains("docker pull"),
            "{error}"
        );
    }

    #[test]
    fn a_docker_that_would_write_as_another_uid_or_in_a_virtual_machine_or_too_old_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let refused = |info: String| {
            check(
                &fake_engine(dir.path(), Engine::Docker, &info, &["localhost/base"]),
                "linux",
            )
            .unwrap_err()
        };
        for option in ["name=rootless", "name=userns"] {
            assert_eq!(
                refused(docker_info("27.3.1", "Fedora Linux 44", &[option])),
                CheckError::RemappedDocker
            );
        }
        assert_eq!(
            refused(docker_info("4.30.0", "Docker Desktop", &[])),
            CheckError::DockerDesktop
        );
        assert_eq!(
            refused(docker_info("19.03.15", "Debian GNU/Linux 12", &[])),
            CheckError::Version {
                engine: Engine::Docker,
                found: "19.03.15".into(),
                minimum: MINIMUM_DOCKER
            }
        );
    }

    fn info(rootless: bool, version: &str, pasta: &str) -> String {
        serde_json::json!({
            "host": {"security": {"rootless": rootless}, "pasta": {"executable": pasta}},
            "version": {"Version": version},
        })
        .to_string()
    }

    #[test]
    fn rootless_podman_with_pasta_and_the_images_passes() {
        let dir = tempfile::tempdir().unwrap();
        let mut settings = fake(
            dir.path(),
            &info(true, "5.8.7", "/usr/bin/pasta"),
            &["localhost/base", "localhost/shop"],
        );
        assert_eq!(check(&settings, "linux"), Ok(()));
        settings
            .repo_images
            .insert("shop".into(), "localhost/shop".into());
        assert_eq!(check(&settings, "linux"), Ok(()));
    }

    #[test]
    fn a_missing_image_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let mut settings = fake(
            dir.path(),
            &info(true, "5.8.7", "/usr/bin/pasta"),
            &["localhost/base"],
        );
        settings
            .repo_images
            .insert("shop".into(), "localhost/shop".into());
        let error = check(&settings, "linux").unwrap_err();
        assert_eq!(
            error,
            CheckError::MissingImage {
                engine: Engine::Podman,
                image: "localhost/shop".into()
            }
        );
        assert!(error.to_string().contains("never pulls"));
    }

    #[test]
    fn rootful_old_or_pastaless_podman_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let rootful = fake(
            dir.path(),
            &info(false, "5.8.7", "/usr/bin/pasta"),
            &["localhost/base"],
        );
        assert_eq!(check(&rootful, "linux"), Err(CheckError::Rootful));
        let old = fake(
            dir.path(),
            &info(true, "4.3.1", "/usr/bin/pasta"),
            &["localhost/base"],
        );
        assert_eq!(
            check(&old, "linux"),
            Err(CheckError::Version {
                engine: Engine::Podman,
                found: "4.3.1".into(),
                minimum: MINIMUM_PODMAN
            })
        );
        // Without pasta in Podman's answer, the PATH decides; the fake PATH holds none.
        let pastaless = fake(dir.path(), &info(true, "5.8.7", ""), &["localhost/base"]);
        if resolve("pasta").is_none() {
            assert_eq!(check(&pastaless, "linux"), Err(CheckError::NoPasta));
        }
    }

    #[test]
    fn an_info_that_is_not_json_is_refused_with_what_podman_said() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("podman");
        crate::testing::script(
            &path,
            "#!/bin/sh\necho 'cannot connect to the service' >&2\nexit 125\n",
        );
        let settings = Settings {
            program: path.to_string_lossy().into_owned(),
            image: Some("base".into()),
            ..Settings::default()
        };
        let error = check(&settings, "linux").unwrap_err();
        assert!(
            error.to_string().contains("cannot connect to the service"),
            "{error}"
        );
    }
}
