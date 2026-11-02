//! `index.toml`: reading it by the rules Moebius reads it by, adding one release to it, writing it, and checking that
//! the one that is committed fits the manifests.

use std::path::Path;

use crate::model::{INDEX_FORMAT, Index, Package};
use crate::packages::{self, Found, entry};
use crate::{Error, Result, read};

const HEADER: &str = "# The package index of moebius-plugins, written by tools/index. Do not edit it by hand: a release adds its\n\
# version here, and the versions of earlier releases stay.\n";

/// The packages of the text of an `index.toml`, which Moebius refuses as a whole when one entry is not valid.
pub fn parse(text: &str) -> Result<Vec<Package>> {
    let index: Index = toml::from_str(text)
        .map_err(|e| Error::Invalid(format!("index.toml is not valid: {e}")))?;
    if index.format != INDEX_FORMAT {
        return Err(Error::Invalid(format!(
            "index.toml has format {}, and Moebius reads format {INDEX_FORMAT}",
            index.format
        )));
    }
    let mut seen = std::collections::BTreeSet::new();
    for package in &index.package {
        package.check_entry()?;
        if !seen.insert((package.name.clone(), package.parsed_version())) {
            return Err(Error::Invalid(format!(
                "{} {} is in the index twice",
                package.name, package.version
            )));
        }
    }
    Ok(index.package)
}

/// The packages of the index at `path`; none when there is no such file.
pub fn read_file(path: &Path) -> Result<Vec<Package>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = String::from_utf8_lossy(&read(path)?).into_owned();
    parse(&text).map_err(|e| Error::Invalid(format!("{}: {e}", path.display())))
}

/// `existing` with `added`: an entry of the same name and version must be the same (a released version does not
/// change; adding it again is what a repeated release run does). Sorted by name, then version.
pub fn merge(mut existing: Vec<Package>, added: Package) -> Result<Vec<Package>> {
    match existing
        .iter()
        .find(|p| p.name == added.name && p.parsed_version() == added.parsed_version())
    {
        Some(same) if *same == added => return Ok(existing),
        Some(_) => {
            return Err(Error::Invalid(format!(
                "{} {} is in the index already, with other contents; a released version does not change, release a new one",
                added.name, added.version
            )));
        }
        None => existing.push(added),
    }
    existing.sort_by(|a, b| (&a.name, a.parsed_version()).cmp(&(&b.name, b.parsed_version())));
    Ok(existing)
}

/// The text of an index with these packages.
pub fn render(packages: Vec<Package>) -> Result<String> {
    let index = Index {
        format: INDEX_FORMAT,
        package: packages,
    };
    let body = toml::to_string(&index)
        .map_err(|e| Error::Invalid(format!("cannot write the index: {e}")))?;
    Ok(format!("{HEADER}\n{body}"))
}

/// Adds the release `tag` to the index at `index_path`, writes it, and returns the entry that was added.
pub fn add(
    found: &[Found],
    tag: &str,
    assets: Option<&Path>,
    index_path: &Path,
) -> Result<Package> {
    let package = packages::for_tag(found, tag)?;
    let added = entry(package, tag, assets)?;
    let merged = merge(read_file(index_path)?, added.clone())?;
    let text = render(merged)?;
    parse(&text)?;
    std::fs::write(index_path, text).map_err(|source| Error::Io {
        path: index_path.to_path_buf(),
        source,
    })?;
    Ok(added)
}

/// Checks the index at `index_path` against the manifests: it must be a valid index; no package may be in it at a
/// version newer than its manifest says; and the entry of the manifest's own version, if it is released, must say what
/// the manifest says and name the files at their tag. A manifest whose version is not in the index is not released yet,
/// and an index that does not exist is a repository without releases: both are fine.
pub fn check(found: &[Found], index_path: &Path) -> Result<()> {
    for entry in read_file(index_path)? {
        let Some(package) = found.iter().find(|f| f.manifest.name == entry.name) else {
            continue;
        };
        let manifest = &package.manifest;
        if entry.parsed_version() > manifest.parsed_version() {
            return Err(Error::Invalid(format!(
                "the index has {} {}, and the manifest of {} says {}",
                entry.name, entry.version, entry.name, manifest.version
            )));
        }
        if entry.version != manifest.version {
            continue;
        }
        let tag = format!("{}-v{}", entry.name, entry.version);
        let mut stripped = entry.clone();
        stripped.downloads.clear();
        let mut expected = manifest.clone();
        for file in &mut expected.files {
            file.source = None;
        }
        for file in &mut stripped.files {
            file.url.clear();
            file.sha256.clear();
        }
        if stripped != expected {
            return Err(Error::Invalid(format!(
                "the manifest of {tag} differs from its entry in the index; a released version does not change, so bump the version"
            )));
        }
        if manifest.kind == "plugin" {
            let mut targets: Vec<&str> =
                entry.downloads.iter().map(|d| d.target.as_str()).collect();
            let mut wanted: Vec<&str> = manifest.targets.iter().map(String::as_str).collect();
            targets.sort_unstable();
            wanted.sort_unstable();
            if targets != wanted {
                return Err(Error::Invalid(format!(
                    "{tag} has downloads for {targets:?} in the index, and its targets are {wanted:?}"
                )));
            }
            let program = &manifest
                .plugin
                .as_ref()
                .map(|p| p.program.clone())
                .unwrap_or_default();
            for download in &entry.downloads {
                let url = format!(
                    "https://github.com/{}/releases/download/{tag}/{}",
                    packages::REPOSITORY,
                    packages::asset_name(program, &download.target)
                );
                if download.url != url {
                    return Err(Error::Invalid(format!(
                        "{tag}: the download for {} is {}, not {url}",
                        download.target, download.url
                    )));
                }
            }
        } else {
            let rebuilt = entry_from_files(package, &tag)?;
            if rebuilt.files != entry.files {
                return Err(Error::Invalid(format!(
                    "the files of {tag} are not what the index says (url or sha256); a released version does not change"
                )));
            }
        }
    }
    Ok(())
}

/// Checks that the programs in `assets_dir` (downloaded back from the release) are the ones the index lists for `tag`:
/// every download of the entry has its file there, with the sha256 the index names. A package that is not a program
/// has nothing to verify.
pub fn verify_assets(index_path: &Path, tag: &str, assets_dir: &Path) -> Result<()> {
    let (name, version) = packages::parse_tag(tag)?;
    let wanted = crate::version::Version::parse(&version);
    let entry = read_file(index_path)?
        .into_iter()
        .find(|p| p.name == name && Some(p.parsed_version()) == wanted)
        .ok_or_else(|| Error::Invalid(format!("{tag} is not in {}", index_path.display())))?;
    for download in &entry.downloads {
        let asset = download.url.rsplit('/').next().unwrap_or_default();
        let path = assets_dir.join(asset);
        let actual = packages::sha256(&read(&path)?);
        if actual != download.sha256 {
            return Err(Error::Invalid(format!(
                "{asset} of the release has sha256 {actual}, and the index says {}",
                download.sha256
            )));
        }
    }
    Ok(())
}

fn entry_from_files(package: &Found, tag: &str) -> Result<Package> {
    entry(package, tag, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packages::discover;
    use crate::packages::tests::{WORKFLOW, repository};

    /// What Moebius's own tests use as a valid index.
    const MOEBIUS_PLUGIN: &str = "format = 1\n[[package]]\nname = \"gate\"\nversion = \"1.0.0\"\nkind = \"plugin\"\n\
        moebius = \">=0.1\"\n[package.plugin]\nkind = \"decider\"\nprogram = \"gate\"\ndecides = [\"start_card\"]\n\
        [[package.downloads]]\ntarget = \"x86_64-unknown-linux-gnu\"\nurl = \"https://example.com/gate\"\n\
        sha256 = \"abababababababababababababababababababababababababababababababab\"\n";

    #[test]
    fn an_index_is_read_whole_or_refused_whole_as_moebius_does() {
        assert_eq!(parse(MOEBIUS_PLUGIN).unwrap().len(), 1);
        assert!(parse("format = 1\n").unwrap().is_empty());
        let error = |text: String| parse(&text).unwrap_err().to_string();
        assert!(error(MOEBIUS_PLUGIN.replace("format = 1", "format = 2")).contains("format 2"));
        assert!(error("format = 1\n[[package]]\nname = 3\n".into()).contains("not valid"));
        let twice = format!(
            "{MOEBIUS_PLUGIN}{}",
            MOEBIUS_PLUGIN.replace("format = 1\n", "")
        );
        assert!(error(twice).contains("twice"));
        assert!(
            error(MOEBIUS_PLUGIN.replace("sha256 = \"abab", "sha256 = \"zz")).contains("sha256")
        );
        assert!(
            error(MOEBIUS_PLUGIN.replace("https://example.com", "ftp://example.com"))
                .contains("URL")
        );
    }

    fn release(root: &Path, tag: &str) -> std::path::PathBuf {
        let index_path = root.join("index.toml");
        let found = discover(root).unwrap();
        add(&found, tag, Some(&root.join("assets")), &index_path).unwrap();
        index_path
    }

    #[test]
    fn a_release_is_written_in_a_form_moebius_reads() {
        let dir = tempfile::tempdir().unwrap();
        repository(dir.path());
        let path = release(dir.path(), "gate-v1.0.0");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# The package index"), "{text}");
        let packages = parse(&text).unwrap();
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].downloads.len(), 2);
        assert_eq!(
            packages[0].plugin.as_ref().unwrap().settings[0].default,
            Some(toml::Value::Array(vec![]))
        );
        // Writing again, with nothing new, changes nothing.
        let again = release(dir.path(), "gate-v1.0.0");
        assert_eq!(std::fs::read_to_string(again).unwrap(), text);
        check(&discover(dir.path()).unwrap(), &path).unwrap();
    }

    #[test]
    fn earlier_releases_stay_and_a_released_version_does_not_change() {
        let dir = tempfile::tempdir().unwrap();
        repository(dir.path());
        let path = release(dir.path(), "gate-v1.0.0");
        let manifest = dir.path().join("plugins/gate/moebius-package.toml");
        let text = std::fs::read_to_string(&manifest).unwrap();
        // The same version with other contents is refused.
        std::fs::write(&manifest, text.replace("Gates.", "Gates more.")).unwrap();
        let error = add(
            &discover(dir.path()).unwrap(),
            "gate-v1.0.0",
            Some(&dir.path().join("assets")),
            &path,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("in the index already"), "{error}");
        // The check says the same of a manifest that changed after its release.
        assert!(
            check(&discover(dir.path()).unwrap(), &path)
                .unwrap_err()
                .to_string()
                .contains("bump the version")
        );
        // A new version is added beside the old one.
        std::fs::write(&manifest, text.replace("1.0.0", "1.1.0")).unwrap();
        release(dir.path(), "gate-v1.1.0");
        let versions: Vec<String> = read_file(&path)
            .unwrap()
            .into_iter()
            .map(|p| p.version)
            .collect();
        assert_eq!(versions, ["1.0.0", "1.1.0"]);
    }

    #[test]
    fn a_tag_that_does_not_match_the_manifest_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        repository(dir.path());
        let path = dir.path().join("index.toml");
        let found = discover(dir.path()).unwrap();
        assert!(
            add(
                &found,
                "gate-v9.9.9",
                Some(&dir.path().join("assets")),
                &path
            )
            .is_err()
        );
        assert!(!path.exists());
    }

    #[test]
    fn the_check_accepts_no_index_and_unreleased_manifests() {
        let dir = tempfile::tempdir().unwrap();
        repository(dir.path());
        let found = discover(dir.path()).unwrap();
        let path = dir.path().join("index.toml");
        check(&found, &path).unwrap();
        std::fs::write(&path, "format = 1\n").unwrap();
        check(&found, &path).unwrap();
        // A manifest ahead of the index is a version not released yet.
        let released = release(dir.path(), "gate-v1.0.0");
        let manifest = dir.path().join("plugins/gate/moebius-package.toml");
        let text = std::fs::read_to_string(&manifest).unwrap();
        std::fs::write(&manifest, text.replace("1.0.0", "1.2.0")).unwrap();
        check(&discover(dir.path()).unwrap(), &released).unwrap();
        // An index ahead of the manifest is not.
        std::fs::write(&manifest, text.replace("1.0.0", "0.9.0")).unwrap();
        let error = check(&discover(dir.path()).unwrap(), &released)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("1.0.0") && error.contains("0.9.0"),
            "{error}"
        );
        std::fs::write(&path, "format = 3\n").unwrap();
        assert!(check(&found, &path).is_err());
    }

    #[test]
    fn files_that_changed_after_their_release_are_caught() {
        let dir = tempfile::tempdir().unwrap();
        let package = dir.path().join("workflows/flow");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join("moebius-package.toml"), WORKFLOW).unwrap();
        std::fs::write(package.join("flow.luau"), "return 1\n").unwrap();
        let path = release(dir.path(), "flow-v2.1");
        let found = discover(dir.path()).unwrap();
        check(&found, &path).unwrap();
        std::fs::write(package.join("flow.luau"), "return 2\n").unwrap();
        assert!(
            check(&found, &path)
                .unwrap_err()
                .to_string()
                .contains("not what the index says")
        );
    }

    /// Edits the released entry of `gate` in the index at `path`.
    fn edit_entry(path: &Path, edit: impl FnOnce(&mut Package)) {
        let mut packages = read_file(path).unwrap();
        edit(&mut packages[0]);
        std::fs::write(path, render(packages).unwrap()).unwrap();
    }

    #[test]
    fn a_released_entry_with_other_downloads_or_urls_is_caught() {
        let dir = tempfile::tempdir().unwrap();
        repository(dir.path());
        let path = release(dir.path(), "gate-v1.0.0");
        let found = discover(dir.path()).unwrap();
        edit_entry(&path, |p| {
            p.downloads.pop();
        });
        let error = check(&found, &path).unwrap_err().to_string();
        assert!(error.contains("has downloads for"), "{error}");

        std::fs::remove_file(&path).unwrap();
        let path = release(dir.path(), "gate-v1.0.0");
        edit_entry(&path, |p| {
            p.downloads[0].url = "https://example.com/gate".into();
        });
        let error = check(&found, &path).unwrap_err().to_string();
        assert!(error.contains("not https://github.com/"), "{error}");
    }

    #[test]
    fn the_programs_of_the_release_must_be_the_ones_the_index_names() {
        let dir = tempfile::tempdir().unwrap();
        repository(dir.path());
        let path = release(dir.path(), "gate-v1.0.0");
        let assets = dir.path().join("assets");
        verify_assets(&path, "gate-v1.0.0", &assets).unwrap();
        // A program replaced on the release (a rebuild that differs) no longer matches the index.
        let program = assets.join("gate-aarch64-apple-darwin");
        std::fs::write(&program, "another build").unwrap();
        let error = verify_assets(&path, "gate-v1.0.0", &assets)
            .unwrap_err()
            .to_string();
        assert!(error.contains("gate-aarch64-apple-darwin"), "{error}");
        std::fs::remove_file(&program).unwrap();
        assert!(verify_assets(&path, "gate-v1.0.0", &assets).is_err());
        assert!(verify_assets(&path, "gate-v2.0.0", &assets).is_err());
    }

    #[test]
    fn a_release_that_the_index_lists_with_other_programs_is_refused_before_anything_is_uploaded() {
        let dir = tempfile::tempdir().unwrap();
        repository(dir.path());
        let path = release(dir.path(), "gate-v1.0.0");
        let before = std::fs::read_to_string(&path).unwrap();
        // The same tag built again, with a different program: what the workflow runs first, on a copy of the index.
        std::fs::write(
            dir.path().join("assets/gate-x86_64-unknown-linux-gnu"),
            "rebuilt",
        )
        .unwrap();
        let found = discover(dir.path()).unwrap();
        let error = add(
            &found,
            "gate-v1.0.0",
            Some(&dir.path().join("assets")),
            &path,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("in the index already"), "{error}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    }

    #[test]
    fn an_index_without_packages_is_written_and_read() {
        let text = render(Vec::new()).unwrap();
        assert!(parse(&text).unwrap().is_empty());
        // The first release adds to it as to any index.
        let dir = tempfile::tempdir().unwrap();
        repository(dir.path());
        let path = dir.path().join("index.toml");
        std::fs::write(&path, text).unwrap();
        let path = release(dir.path(), "gate-v1.0.0");
        assert_eq!(read_file(&path).unwrap().len(), 1);
    }
}
