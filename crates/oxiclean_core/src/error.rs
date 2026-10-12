//! Errors returned by the oxiclean libraries.

use std::{io, path::PathBuf};

/// Everything that can stop an analysis before it produces results.
///
/// Problems with individual files (unreadable or unparseable sources) are not
/// errors: they are collected as [`crate::ParseFailure`]s and reported
/// alongside the results.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("could not read the current directory")]
    CurrentDir(#[source] io::Error),

    #[error("no .git directory found in {} or any parent; pass --root", .0.display())]
    GitRootNotFound(PathBuf),

    #[error("root directory {} is not accessible", path.display())]
    Root {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("invalid entry glob '{glob}'")]
    InvalidGlob {
        glob: String,
        #[source]
        source: globset::Error,
    },

    #[error("failed to walk {}", path.display())]
    Walk {
        path: PathBuf,
        #[source]
        source: ignore::Error,
    },

    #[error("no entry files found under {}", .0.display())]
    NoEntries(PathBuf),
}

/// Shorthand for results in this crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;
