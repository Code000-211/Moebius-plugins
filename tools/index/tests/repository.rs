//! The generator against this repository's own packages: every manifest is valid, and the nono sandbox, released with
//! stand-in programs, gives an index that passes the checks.

use std::path::PathBuf;

use moebius_plugins_index::{index, packages};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn every_manifest_of_the_repository_is_valid_and_can_be_released() {
    let found = packages::discover(&root()).unwrap();
    let nono = found
        .iter()
        .find(|f| f.manifest.name == "nono-sandbox")
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let tag = format!("nono-sandbox-v{}", nono.manifest.version);
    let assets = dir.path().join("assets");
    std::fs::create_dir(&assets).unwrap();
    for target in &nono.manifest.targets {
        std::fs::write(
            assets.join(packages::asset_name("nono-sandbox", target)),
            target,
        )
        .unwrap();
    }
    let path = dir.path().join("index.toml");
    index::add(&found, &tag, Some(&assets), &path).unwrap();
    index::check(&found, &path).unwrap();
    let listed = index::read_file(&path).unwrap();
    assert_eq!(listed[0].downloads.len(), nono.manifest.targets.len());
    assert!(
        listed[0]
            .downloads
            .iter()
            .all(|d| !d.target.contains("windows"))
    );
}

#[test]
fn the_committed_index_fits_the_manifests() {
    let found = packages::discover(&root()).unwrap();
    let path = root().join("index.toml");
    // The file is committed (an empty index until the first release), so that Moebius reads the registry cleanly.
    assert!(path.exists(), "index.toml is committed");
    index::read_file(&path).unwrap();
    index::check(&found, &path).unwrap();
}

/// The text of the workflows, which the workflow tests below read.
fn workflow(name: &str) -> String {
    std::fs::read_to_string(root().join(".github/workflows").join(name)).unwrap()
}

#[test]
fn the_release_workflow_never_overwrites_a_program_and_works_out_the_entry_first() {
    let release = workflow("release.yml");
    assert!(!release.contains("--clobber"));
    let position = |needle: &str| release.find(needle).unwrap_or_else(|| panic!("{needle}"));
    // The merge into a copy of the index (which refuses a version listed with other contents) is before the upload,
    // and the check of the uploaded bytes and of the entry are before the commit to main.
    let entry = position("Work out the entry of the index");
    let upload = position("gh release upload");
    let verify = position("verify \"$TAG\"");
    let commit = position("Commit index.toml to main");
    assert!(entry < upload && upload < verify && verify < commit);
    assert!(position("check --root release --index work-index.toml") < upload);
}

#[test]
fn the_release_builds_the_commit_the_tag_names_and_writes_nothing_before_the_release_job() {
    let release = workflow("release.yml");
    assert!(release.contains("is-ancestor \"$GITHUB_SHA\" origin/main"));
    assert!(!release.contains("github.ref }}"), "one commit: github.sha");
    assert!(release.starts_with("name: Release"));
    assert!(release.contains("\npermissions:\n  contents: read\n"));
}

#[test]
fn the_workflows_build_with_the_toolchain_the_repository_pins() {
    let pinned = std::fs::read_to_string(root().join("rust-toolchain.toml")).unwrap();
    let channel = pinned
        .lines()
        .find_map(|l| l.strip_prefix("channel = "))
        .unwrap()
        .trim_matches('"')
        .to_string();
    for name in ["ci.yml", "release.yml"] {
        let text = workflow(name);
        assert!(!text.contains("rust-toolchain@stable"), "{name}");
        for line in text
            .lines()
            .filter(|l| l.contains("dtolnay/rust-toolchain@"))
        {
            assert!(line.ends_with(&format!("@{channel}")), "{name}: {line}");
        }
    }
}
