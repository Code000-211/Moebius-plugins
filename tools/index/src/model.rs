//! A package as `moebius-package.toml` says it and as an entry of `index.toml` says it: the index entry is the
//! manifest plus the download of each target (plugins) or the URL and sha256 of each file (the others). The fields and
//! the checks are those of Moebius's package manager, so that an index written here is accepted there.

use serde::{Deserialize, Serialize};

use crate::version::{Version, valid_range};
use crate::{Error, Result};

/// The format of `index.toml` that Moebius reads.
pub const INDEX_FORMAT: u32 = 1;

/// The kinds of package, with the folder of this repository each one lives in.
pub const KINDS: [(&str, &str); 4] = [
    ("plugin", "plugins"),
    ("workflow", "workflows"),
    ("rules", "rules"),
    ("skills", "skills"),
];

/// An `index.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Index {
    pub format: u32,
    #[serde(default)]
    pub package: Vec<Package>,
}

/// One version of a package: a manifest, or an entry of the index.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Package {
    pub name: String,
    pub version: String,
    pub kind: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    pub moebius: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin: Option<PluginSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub downloads: Vec<Download>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<PackageFile>,
}

/// What a plugin is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginSpec {
    pub kind: String,
    pub program: String,
    #[serde(default)]
    pub launches: bool,
    #[serde(default)]
    pub decides: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings_as: Option<String>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub requires: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub settings: Vec<Setting>,
}

/// One setting of a plugin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Setting {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flag: Option<String>,
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<String>,
    #[serde(default)]
    pub repeatable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<toml::Value>,
    #[serde(default)]
    pub description: String,
}

/// The program of a plugin for one target.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Download {
    pub target: String,
    pub url: String,
    pub sha256: String,
}

/// A file of a workflow, rules or skills package.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PackageFile {
    /// The file in the package's folder (manifest only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Where it goes.
    pub to: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sha256: String,
}

fn invalid<T>(package: &Package, why: &str) -> Result<T> {
    Err(Error::Invalid(format!(
        "{} {}: {why}",
        package.name, package.version
    )))
}

/// Whether `name` is a package name: letters, digits, `-` and `_`.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Whether `path` is a plain relative path: no `..`, no empty or `.` parts, no backslash, nothing that names `.git`.
pub fn plain_relative(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\\')
        && path.split('/').all(|part| {
            let name = part.split(':').next().unwrap_or("");
            let name = name.trim_end_matches(['.', ' ']).to_ascii_lowercase();
            let short = name
                .strip_prefix("git~")
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
            !part.is_empty() && part != "." && part != ".." && name != ".git" && !short
        })
}

impl Package {
    /// The version, which a checked package has.
    pub fn parsed_version(&self) -> Version {
        Version::parse(&self.version).unwrap_or(Version::parse("0").expect("0 is a version"))
    }

    /// Whether this is an entry the index can offer, by the rules Moebius applies when it reads the index.
    pub fn check_entry(&self) -> Result<()> {
        self.check_common()?;
        let download = |url: &str, sha256: &str| -> Result<()> {
            if !(url.starts_with("https://") || url.starts_with("http://")) {
                return invalid(self, "a URL is not http or https");
            }
            if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
                return invalid(self, "a sha256 is not 64 hexadecimal digits");
            }
            Ok(())
        };
        if self.kind == "plugin" {
            if self.downloads.is_empty() {
                return invalid(self, "a plugin needs a download for each target");
            }
            for each in &self.downloads {
                download(&each.url, &each.sha256)?;
            }
            return Ok(());
        }
        if self.files.is_empty() {
            return invalid(self, "it has no files");
        }
        for file in &self.files {
            download(&file.url, &file.sha256)?;
        }
        Ok(())
    }

    /// Whether this is a manifest that can be released: valid fields, and files that say where they come from.
    pub fn check_manifest(&self) -> Result<()> {
        self.check_common()?;
        if !self.downloads.is_empty() {
            return invalid(
                self,
                "a manifest has no downloads; the generator writes them",
            );
        }
        if self.kind == "plugin" {
            if self.targets.is_empty() {
                return invalid(self, "a plugin needs `targets`");
            }
            return Ok(());
        }
        for file in &self.files {
            if !file.url.is_empty() || !file.sha256.is_empty() {
                return invalid(self, "a manifest's files have no url or sha256");
            }
            match file.source.as_deref() {
                Some(source) if plain_relative(source) => {}
                _ => return invalid(self, "`source` is not a plain relative path"),
            }
        }
        Ok(())
    }

    /// The checks of Moebius that manifest and entry share.
    fn check_common(&self) -> Result<()> {
        if !valid_name(&self.name) {
            return Err(Error::Invalid(format!(
                "{:?} is not a package name (letters, digits, - and _)",
                self.name
            )));
        }
        if Version::parse(&self.version).is_none() {
            return invalid(self, "the version is not numbers separated by dots");
        }
        if !valid_range(&self.moebius) {
            return invalid(self, "`moebius` is not a range of versions");
        }
        if !KINDS.iter().any(|(kind, _)| *kind == self.kind) {
            return invalid(self, "`kind` is not plugin, workflow, rules or skills");
        }
        if self.kind == "plugin" {
            let Some(plugin) = &self.plugin else {
                return invalid(self, "a plugin needs a [plugin] table");
            };
            if !matches!(plugin.kind.as_str(), "decider" | "launcher") {
                return invalid(self, "`plugin.kind` is not decider or launcher");
            }
            if plugin.kind == "decider" && plugin.decides.is_empty() {
                return invalid(self, "a decider needs `plugin.decides`");
            }
            let program = &plugin.program;
            if program.is_empty()
                || program.contains(['/', '\\', ':'])
                || program.starts_with('.')
                || program.starts_with('-')
            {
                return invalid(self, "`plugin.program` is not a file name");
            }
            return Ok(());
        }
        if self.kind == "rules" && self.files.len() != 1 {
            return invalid(self, "a rules package has exactly one file");
        }
        for file in &self.files {
            if !plain_relative(&file.to) {
                return invalid(self, "`to` is not a plain relative path");
            }
            if self.kind == "workflow" && !file.to.ends_with(".luau") {
                return invalid(self, "a workflow's files are column scripts (.luau)");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) const MANIFEST: &str = r#"
name = "gate"
version = "1.0.0"
kind = "plugin"
description = "Gates."
moebius = ">=0.2"
targets = ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"]

[plugin]
kind = "decider"
program = "gate"
decides = ["start_card"]

[[plugin.settings]]
name = "mode"
flag = "--mode"
type = "string"
default = []
description = "How."
"#;

    pub(crate) fn manifest(text: &str) -> Package {
        toml::from_str(text).unwrap()
    }

    #[test]
    fn a_manifest_is_checked_by_the_rules_of_moebius() {
        manifest(MANIFEST).check_manifest().unwrap();
        let refused = |from: &str, to: &str| {
            manifest(&MANIFEST.replace(from, to))
                .check_manifest()
                .unwrap_err()
                .to_string()
        };
        assert!(refused("name = \"gate\"", "name = \"a b\"").contains("package name"));
        assert!(refused("version = \"1.0.0\"", "version = \"1.x\"").contains("version"));
        assert!(refused("\">=0.2\"", "\"^1\"").contains("range"));
        assert!(refused("kind = \"plugin\"\ndesc", "kind = \"theme\"\ndesc").contains("kind"));
        assert!(refused("program = \"gate\"", "program = \"../gate\"").contains("program"));
        assert!(refused("decides = [\"start_card\"]", "decides = []").contains("decides"));
    }

    #[test]
    fn paths_stay_inside_their_folder() {
        for good in ["a.luau", "dir/a.luau", "tdd/SKILL.md"] {
            assert!(plain_relative(good), "{good}");
        }
        for bad in [
            "", "/a", "../a", "a/../b", "a//b", "./a", "a\\b", ".git/x", ".GIT/x",
        ] {
            assert!(!plain_relative(bad), "{bad}");
        }
    }
}
