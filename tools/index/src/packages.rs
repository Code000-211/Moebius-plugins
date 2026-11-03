//! The packages of this repository: finding their manifests, reading a release tag, and turning a manifest into the
//! entry of the index for one release (the download URLs and sha256 of the built programs, or of the tagged files).

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::model::{Download, KINDS, Package};
use crate::version::Version;
use crate::{Error, Result, read};

/// The GitHub repository the releases and the files are in.
pub const REPOSITORY: &str = "Code000-211/moebius-plugins";

/// A package found in the repository.
#[derive(Debug, Clone)]
pub struct Found {
    /// Its folder, such as `plugins/nono-sandbox`.
    pub dir: PathBuf,
    /// Its folder from the repository's root, with `/`.
    pub relative: String,
    pub manifest: Package,
}

/// The sha256 of `bytes`, as 64 lowercase hexadecimal digits.
pub fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Every package under `plugins/`, `workflows/`, `rules/` and `skills/` of `root`, by name. A folder without a
/// `moebius-package.toml` (a README's folder) is not a package; a manifest that is not valid, or that is in the wrong
/// folder, is an error.
pub fn discover(root: &Path) -> Result<Vec<Found>> {
    let mut found: Vec<Found> = Vec::new();
    for (kind, folder) in KINDS {
        let Ok(entries) = std::fs::read_dir(root.join(folder)) else {
            continue;
        };
        for entry in entries {
            let dir = entry
                .map_err(|source| Error::Io {
                    path: root.join(folder),
                    source,
                })?
                .path();
            let manifest_path = dir.join("moebius-package.toml");
            if !manifest_path.is_file() {
                continue;
            }
            let text = String::from_utf8_lossy(&read(&manifest_path)?).into_owned();
            let manifest: Package = toml::from_str(&text).map_err(|e| Error::Toml {
                path: manifest_path.clone(),
                message: e.to_string(),
            })?;
            manifest.check_manifest()?;
            let name = dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if manifest.name != name {
                return Err(Error::Invalid(format!(
                    "{}: the name {:?} is not the name of its folder",
                    manifest_path.display(),
                    manifest.name
                )));
            }
            if manifest.kind != kind {
                return Err(Error::Invalid(format!(
                    "{}: a package of kind {} belongs in {folder}/, not here",
                    manifest_path.display(),
                    manifest.kind
                )));
            }
            check_crate_version(&dir, &manifest)?;
            if found.iter().any(|f| f.manifest.name == name) {
                return Err(Error::Invalid(format!("the package {name} is there twice")));
            }
            found.push(Found {
                dir,
                relative: format!("{folder}/{name}"),
                manifest,
            });
        }
    }
    found.sort_by(|a, b| a.manifest.name.cmp(&b.manifest.name));
    Ok(found)
}

/// A plugin written in Rust reports its crate's version, so the version of its `Cargo.toml` must be the manifest's.
fn check_crate_version(dir: &Path, manifest: &Package) -> Result<()> {
    let cargo = dir.join("Cargo.toml");
    if manifest.plugin.is_none() || !cargo.is_file() {
        return Ok(());
    }
    let text = String::from_utf8_lossy(&read(&cargo)?).into_owned();
    let parsed: toml::Table = toml::from_str(&text).map_err(|e| Error::Toml {
        path: cargo.clone(),
        message: e.to_string(),
    })?;
    // A version inherited from the workspace is not a literal one to compare.
    let version = parsed
        .get("package")
        .and_then(|p| p.get("version"))
        .and_then(|v| v.as_str());
    match version {
        Some(version) if version != manifest.version => Err(Error::Invalid(format!(
            "{}: the version is {version}, and the manifest {} says {}; set both to the same version",
            cargo.display(),
            dir.join("moebius-package.toml").display(),
            manifest.version
        ))),
        _ => Ok(()),
    }
}

/// The package name and version of a release tag `<name>-v<version>`.
pub fn parse_tag(tag: &str) -> Result<(String, String)> {
    let parsed = tag
        .rsplit_once("-v")
        .filter(|(name, version)| {
            crate::model::valid_name(name) && Version::parse(version).is_some()
        })
        .map(|(name, version)| (name.to_string(), version.to_string()));
    parsed.ok_or_else(|| {
        Error::Invalid(format!(
            "{tag:?} is not a release tag; it is <package>-v<version>, such as nono-sandbox-v0.1.0"
        ))
    })
}

/// The package a release tag names, which must have exactly the version of the tag in its manifest.
pub fn for_tag<'a>(found: &'a [Found], tag: &str) -> Result<&'a Found> {
    let (name, version) = parse_tag(tag)?;
    let package = found
        .iter()
        .find(|f| f.manifest.name == name)
        .ok_or_else(|| {
            Error::Invalid(format!("the tag {tag} names {name}, which is no package"))
        })?;
    if package.manifest.version != version {
        return Err(Error::Invalid(format!(
            "the tag {tag} says version {version}, and the manifest of {name} says {}",
            package.manifest.version
        )));
    }
    Ok(package)
}

/// The name of the release asset that holds a plugin's program for `target`.
pub fn asset_name(program: &str, target: &str) -> String {
    format!("{program}-{target}")
}

/// The index entry for the release `tag` of `package`. A plugin's downloads are its release assets, which must all be
/// in `assets`; the other kinds name the files as they are at the tag, and need the files in the package's folder.
pub fn entry(package: &Found, tag: &str, assets: Option<&Path>) -> Result<Package> {
    let mut entry = package.manifest.clone();
    if let Some(plugin) = &package.manifest.plugin {
        let assets = assets.ok_or_else(|| {
            Error::Invalid("a plugin's release needs --assets, its programs".into())
        })?;
        for target in &package.manifest.targets {
            let asset = asset_name(&plugin.program, target);
            let path = assets.join(&asset);
            let bytes = read(&path)?;
            if bytes.is_empty() {
                return Err(Error::Invalid(format!("{} is empty", path.display())));
            }
            entry.downloads.push(Download {
                target: target.clone(),
                url: format!("https://github.com/{REPOSITORY}/releases/download/{tag}/{asset}"),
                sha256: sha256(&bytes),
            });
        }
    }
    for file in &mut entry.files {
        let source = file.source.take().unwrap_or_default();
        file.url = format!(
            "https://raw.githubusercontent.com/{REPOSITORY}/{tag}/{}/{source}",
            package.relative
        );
        file.sha256 = sha256(&read(&package.dir.join(&source))?);
    }
    entry.check_entry()?;
    Ok(entry)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::model::tests::MANIFEST;

    /// A repository root with the package `gate` and its programs in `<root>/assets`.
    pub(crate) fn repository(root: &Path) {
        let dir = root.join("plugins/gate");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("moebius-package.toml"), MANIFEST).unwrap();
        std::fs::create_dir_all(root.join("workflows")).unwrap();
        std::fs::write(root.join("workflows/README.md"), "no package here\n").unwrap();
        let assets = root.join("assets");
        std::fs::create_dir_all(&assets).unwrap();
        for target in ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"] {
            std::fs::write(
                assets.join(asset_name("gate", target)),
                format!("gate {target}"),
            )
            .unwrap();
        }
    }

    pub(crate) const WORKFLOW: &str = "name = \"flow\"\nversion = \"2.1\"\nkind = \"workflow\"\n\
        description = \"A flow.\"\nmoebius = \"*\"\n[[files]]\nsource = \"flow.luau\"\nto = \"flow.luau\"\n";

    #[test]
    fn tags_name_a_package_and_its_version() {
        assert_eq!(
            parse_tag("nono-sandbox-v0.1.0").unwrap(),
            ("nono-sandbox".into(), "0.1.0".into())
        );
        assert_eq!(parse_tag("a-v1").unwrap(), ("a".into(), "1".into()));
        for bad in [
            "v1.0.0",
            "gate-1.0.0",
            "gate-v",
            "gate-v1.x",
            "-v1",
            "a b-v1",
        ] {
            assert!(parse_tag(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_tag_must_match_the_manifest() {
        let dir = tempfile::tempdir().unwrap();
        repository(dir.path());
        let found = discover(dir.path()).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(
            for_tag(&found, "gate-v1.0.0").unwrap().relative,
            "plugins/gate"
        );
        let mismatch = for_tag(&found, "gate-v1.1.0").unwrap_err().to_string();
        assert!(
            mismatch.contains("says version 1.1.0") && mismatch.contains("1.0.0"),
            "{mismatch}"
        );
        assert!(
            for_tag(&found, "other-v1.0.0")
                .unwrap_err()
                .to_string()
                .contains("no package")
        );
    }

    #[test]
    fn a_manifest_in_the_wrong_place_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        repository(dir.path());
        let wrong = dir.path().join("rules/gate");
        std::fs::create_dir_all(&wrong).unwrap();
        std::fs::write(wrong.join("moebius-package.toml"), MANIFEST).unwrap();
        assert!(
            discover(dir.path())
                .unwrap_err()
                .to_string()
                .contains("belongs in")
        );
        std::fs::remove_dir_all(dir.path().join("rules")).unwrap();
        std::fs::rename(
            dir.path().join("plugins/gate"),
            dir.path().join("plugins/other"),
        )
        .unwrap();
        assert!(
            discover(dir.path())
                .unwrap_err()
                .to_string()
                .contains("name of its folder")
        );
    }

    #[test]
    fn a_plugin_whose_cargo_version_differs_from_its_manifest_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        repository(dir.path());
        let cargo = dir.path().join("plugins/gate/Cargo.toml");
        std::fs::write(&cargo, "[package]\nname = \"gate\"\nversion = \"0.9.0\"\n").unwrap();
        let error = discover(dir.path()).unwrap_err().to_string();
        assert!(
            error.contains("the version is 0.9.0") && error.contains("says 1.0.0"),
            "{error}"
        );
        std::fs::write(&cargo, "[package]\nname = \"gate\"\nversion = \"1.0.0\"\n").unwrap();
        discover(dir.path()).unwrap();
    }

    #[test]
    fn a_plugin_release_names_its_assets_with_their_sha256() {
        let dir = tempfile::tempdir().unwrap();
        repository(dir.path());
        let found = discover(dir.path()).unwrap();
        let entry = entry(&found[0], "gate-v1.0.0", Some(&dir.path().join("assets"))).unwrap();
        assert_eq!(entry.downloads.len(), 2);
        assert_eq!(
            entry.downloads[0].url,
            "https://github.com/Code000-211/moebius-plugins/releases/download/gate-v1.0.0/gate-x86_64-unknown-linux-gnu"
        );
        assert_eq!(
            entry.downloads[0].sha256,
            sha256(b"gate x86_64-unknown-linux-gnu")
        );
        assert_eq!(entry.plugin, found[0].manifest.plugin);
    }

    #[test]
    fn a_release_needs_every_program_of_its_targets() {
        let dir = tempfile::tempdir().unwrap();
        repository(dir.path());
        let found = discover(dir.path()).unwrap();
        let assets = dir.path().join("assets");
        std::fs::remove_file(assets.join("gate-aarch64-apple-darwin")).unwrap();
        let error = entry(&found[0], "gate-v1.0.0", Some(&assets))
            .unwrap_err()
            .to_string();
        assert!(error.contains("gate-aarch64-apple-darwin"), "{error}");
        assert!(
            entry(&found[0], "gate-v1.0.0", None)
                .unwrap_err()
                .to_string()
                .contains("--assets")
        );
    }

    #[test]
    fn a_file_package_names_its_files_at_the_tag() {
        let dir = tempfile::tempdir().unwrap();
        let package = dir.path().join("workflows/flow");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join("moebius-package.toml"), WORKFLOW).unwrap();
        std::fs::write(package.join("flow.luau"), "return 1\n").unwrap();
        let found = discover(dir.path()).unwrap();
        let entry = entry(&found[0], "flow-v2.1", None).unwrap();
        assert_eq!(entry.files[0].source, None);
        assert_eq!(
            entry.files[0].url,
            "https://raw.githubusercontent.com/Code000-211/moebius-plugins/flow-v2.1/workflows/flow/flow.luau"
        );
        assert_eq!(entry.files[0].sha256, sha256(b"return 1\n"));
        std::fs::remove_file(package.join("flow.luau")).unwrap();
        assert!(super::entry(&found[0], "flow-v2.1", None).is_err());
    }
}
