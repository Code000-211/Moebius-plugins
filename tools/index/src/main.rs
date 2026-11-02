//! The command line of the generator: `plan <tag>` prints what a release needs as JSON, `add <tag>` adds the release to
//! `index.toml`, `verify <tag>` checks downloaded release programs against the index, `check` checks the committed `index.toml`
//! against the manifests. See the README of this repository.

use std::path::PathBuf;
use std::process::ExitCode;

use moebius_plugins_index::{Error, Result, index, packages, plan};

const USAGE: &str = "usage: moebius-plugins-index plan <tag> [--root DIR]\n\
       moebius-plugins-index add <tag> [--root DIR] [--index FILE] [--assets DIR]\n\
       moebius-plugins-index verify <tag> --index FILE --assets DIR\n\
       moebius-plugins-index check [--root DIR] [--index FILE]";

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Vec<String>) -> Result<()> {
    let usage = || Error::Invalid(USAGE.to_string());
    let mut args = args.into_iter();
    let command = args.next().ok_or_else(usage)?;
    let (mut tag, mut root, mut index_path, mut assets) = (None, PathBuf::from("."), None, None);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--root" => root = args.next().map(PathBuf::from).ok_or_else(usage)?,
            "--index" => index_path = Some(args.next().map(PathBuf::from).ok_or_else(usage)?),
            "--assets" => assets = Some(args.next().map(PathBuf::from).ok_or_else(usage)?),
            _ if !arg.starts_with("--") && tag.is_none() => tag = Some(arg),
            _ => return Err(usage()),
        }
    }
    let index_path = index_path.unwrap_or_else(|| root.join("index.toml"));
    let found = packages::discover(&root)?;
    match (command.as_str(), tag) {
        ("plan", Some(tag)) => {
            let plan = plan::plan(&found, &tag)?;
            println!(
                "{}",
                serde_json::to_string(&plan).map_err(|e| Error::Invalid(e.to_string()))?
            );
        }
        ("add", Some(tag)) => {
            let added = index::add(&found, &tag, assets.as_deref(), &index_path)?;
            println!(
                "added {} {} to {}",
                added.name,
                added.version,
                index_path.display()
            );
        }
        ("verify", Some(tag)) => {
            let assets = assets.ok_or_else(usage)?;
            index::verify_assets(&index_path, &tag, &assets)?;
            println!("the programs in {} are those {tag} lists", assets.display());
        }
        ("check", None) => {
            index::check(&found, &index_path)?;
            println!(
                "{} fits the manifests of {} packages",
                index_path.display(),
                found.len()
            );
        }
        _ => return Err(usage()),
    }
    Ok(())
}
