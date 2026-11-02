//! The generator of `index.toml`, the package index that Moebius reads from this repository (Moebius issue #172).
//!
//! It reads the `moebius-package.toml` of every package under `plugins/`, `workflows/`, `rules/` and `skills/`, and,
//! for one release, the programs that were built or the files that are tagged, and adds that version to the index.
//! Versions of earlier releases stay. What the index must look like is the format of Moebius's package manager
//! (`docs/reference/packages.md` in the Moebius repository); [`model`] checks an entry by the same rules as Moebius
//! does, so an index that passes here is one Moebius accepts.

pub mod index;
pub mod model;
pub mod packages;
pub mod plan;
mod version;

use std::path::PathBuf;

/// Everything that can go wrong while reading packages or writing the index.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path} is not valid: {message}")]
    Toml { path: PathBuf, message: String },
    #[error("{0}")]
    Invalid(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Reads a file, naming it in the error.
pub(crate) fn read(path: &std::path::Path) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })
}
