//! What a release of a tag needs, as JSON for the release workflow: the package, its kind and program, and for a plugin
//! the targets with the runner that builds each.

use serde::Serialize;

use crate::packages::{self, Found};
use crate::{Error, Result};

/// One build of a plugin.
#[derive(Debug, PartialEq, Serialize)]
pub struct Build {
    pub target: String,
    pub runner: String,
}

/// A release.
#[derive(Debug, PartialEq, Serialize)]
pub struct Plan {
    pub tag: String,
    pub name: String,
    pub version: String,
    pub kind: String,
    /// The folder of the package.
    pub dir: String,
    /// The program of a plugin, empty for the others.
    pub program: String,
    /// The builds of a plugin; none for the others.
    pub builds: Vec<Build>,
}

/// The GitHub runner that builds for `target`: the target's own system, so that nothing is cross-compiled but the
/// second architecture of macOS.
pub fn runner(target: &str) -> Result<&'static str> {
    Ok(match target {
        "x86_64-unknown-linux-gnu" => "ubuntu-latest",
        "aarch64-unknown-linux-gnu" => "ubuntu-24.04-arm",
        "x86_64-apple-darwin" | "aarch64-apple-darwin" => "macos-latest",
        "x86_64-pc-windows-msvc" => "windows-latest",
        "aarch64-pc-windows-msvc" => "windows-11-arm",
        other => {
            return Err(Error::Invalid(format!(
                "no runner is known that builds for {other}"
            )));
        }
    })
}

/// The release of `tag`, whose version must be the manifest's.
pub fn plan(found: &[Found], tag: &str) -> Result<Plan> {
    let package = packages::for_tag(found, tag)?;
    let manifest = &package.manifest;
    let mut builds = Vec::new();
    for target in &manifest.targets {
        if manifest.kind == "plugin" {
            builds.push(Build {
                target: target.clone(),
                runner: runner(target)?.to_string(),
            });
        }
    }
    Ok(Plan {
        tag: tag.to_string(),
        name: manifest.name.clone(),
        version: manifest.version.clone(),
        kind: manifest.kind.clone(),
        dir: package.relative.clone(),
        program: manifest
            .plugin
            .as_ref()
            .map(|p| p.program.clone())
            .unwrap_or_default(),
        builds,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packages::{discover, tests::repository};

    #[test]
    fn a_plugin_is_built_on_the_runner_of_each_target() {
        let dir = tempfile::tempdir().unwrap();
        repository(dir.path());
        let plan = plan(&discover(dir.path()).unwrap(), "gate-v1.0.0").unwrap();
        assert_eq!(
            (plan.name.as_str(), plan.program.as_str(), plan.dir.as_str()),
            ("gate", "gate", "plugins/gate")
        );
        let builds: Vec<(&str, &str)> = plan
            .builds
            .iter()
            .map(|b| (b.target.as_str(), b.runner.as_str()))
            .collect();
        assert_eq!(
            builds,
            [
                ("x86_64-unknown-linux-gnu", "ubuntu-latest"),
                ("aarch64-apple-darwin", "macos-latest")
            ]
        );
        assert!(serde_json::to_string(&plan).unwrap().contains("\"builds\""));
    }

    #[test]
    fn a_target_without_a_runner_is_refused() {
        assert!(runner("riscv64gc-unknown-linux-gnu").is_err());
        assert_eq!(
            runner("aarch64-unknown-linux-gnu").unwrap(),
            "ubuntu-24.04-arm"
        );
    }
}
