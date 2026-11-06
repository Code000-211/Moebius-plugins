//! The checks made once when the plugin starts: the platform is Linux, Podman is there, rootless, recent enough and
//! has pasta, and every image the settings name is in its storage. Settings may name no image, since each repository can
//! bring its own Dockerfile; a run that ends up with none is refused at its launch. A plugin that fails them exits with the sentence,
//! so the core marks it not running and every launch is refused: a run never starts outside a container because
//! the container cannot be made.

use std::process::Command;

use serde_json::Value;

use crate::settings::Settings;

/// The oldest Podman whose `--network pasta:<options>` passes the options through to pasta.
pub const MINIMUM_PODMAN: (u32, u32) = (4, 4);

/// Why the plugin cannot start.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CheckError {
    #[error(
        "this platform ({0}) is not one this plugin runs on: only Linux, where rootless Podman runs containers \
         without a virtual machine"
    )]
    Platform(String),
    #[error("cannot run {program:?}: {why}. Install Podman or set --podman to its path")]
    NotFound { program: String, why: String },
    #[error("`{program} info` failed or did not print JSON: {said}")]
    NoInfo { program: String, said: String },
    #[error(
        "Podman runs as root here; this plugin needs rootless Podman, so that a run's container is the user's and \
         not root's"
    )]
    Rootful,
    #[error(
        "Podman {found} is older than {}.{}, the first that passes pasta's options through",
        MINIMUM_PODMAN.0,
        MINIMUM_PODMAN.1
    )]
    Version { found: String },
    #[error(
        "Podman has no pasta, which makes the container's network (only the router's ports in mode \"moebius\"). \
         Install pasta (the package passt)"
    )]
    NoPasta,
    #[error(
        "the image {0:?} is not in Podman's storage; the plugin never pulls one. Build or pull it (`podman pull`), \
         then restart the server"
    )]
    MissingImage(String),
}

/// Checks the platform, Podman and the images. `os` is `std::env::consts::OS`.
pub fn check(settings: &Settings, os: &str) -> Result<(), CheckError> {
    if os != "linux" {
        return Err(CheckError::Platform(os.to_string()));
    }
    let images = settings.images();
    let output = podman(settings, &["info", "--format", "json"])?;
    let info: Value = output
        .success
        .then(|| serde_json::from_str(&output.stdout).ok())
        .flatten()
        .ok_or_else(|| CheckError::NoInfo {
            program: settings.podman.clone(),
            said: last_line(&output.stderr),
        })?;
    if info["host"]["security"]["rootless"] != Value::Bool(true) {
        return Err(CheckError::Rootful);
    }
    let found = info["version"]["Version"]
        .as_str()
        .unwrap_or("")
        .to_string();
    if version(&found).is_none_or(|version| version < MINIMUM_PODMAN) {
        return Err(CheckError::Version { found });
    }
    // Podman 5 says where its pasta is; Podman 4 finds it on the PATH.
    let pasta = info["host"]["pasta"]["executable"]
        .as_str()
        .is_some_and(|path| !path.is_empty())
        || resolve("pasta").is_some();
    if !pasta {
        return Err(CheckError::NoPasta);
    }
    for image in images {
        if !podman(settings, &["image", "exists", image])?.success {
            return Err(CheckError::MissingImage(image.to_string()));
        }
    }
    Ok(())
}

/// The major and minor version of a Podman version such as `5.8.7` or `4.9.3-dev`.
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

fn podman(settings: &Settings, args: &[&str]) -> Result<Ran, CheckError> {
    let output = Command::new(&settings.podman)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| CheckError::NotFound {
            program: settings.podman.clone(),
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
    }

    #[test]
    fn other_platforms_are_refused_before_podman_is_run() {
        let settings = Settings {
            podman: "/does/not/exist".into(),
            image: Some("base".into()),
            ..Settings::default()
        };
        assert_eq!(
            check(&settings, "macos"),
            Err(CheckError::Platform("macos".into()))
        );
        // No image is fine: the repositories may bring Dockerfiles. Podman is still checked.
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
        assert!(error.to_string().contains("--podman"));
    }
}

// The fake Podman is a shell script.
#[cfg(all(test, unix))]
mod podman_tests {
    use std::path::Path;

    use super::*;

    /// A fake Podman whose `info` prints `info` and whose `image exists` succeeds for the images in `images`.
    fn fake(dir: &Path, info: &str, images: &[&str]) -> Settings {
        let path = dir.join("podman");
        let known = images.join(" ");
        let script = format!(
            "#!/bin/sh\ncase \"$1\" in\n  info) cat <<'EOF'\n{info}\nEOF\n;;\n  image) for i in {known}; do [ \"$3\" = \"$i\" ] && exit 0; done; exit 1 ;;\nesac\n"
        );
        crate::testing::script(&path, &script);
        Settings {
            podman: path.to_string_lossy().into_owned(),
            image: Some("localhost/base".into()),
            ..Settings::default()
        }
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
        assert_eq!(error, CheckError::MissingImage("localhost/shop".into()));
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
                found: "4.3.1".into()
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
            podman: path.to_string_lossy().into_owned(),
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
