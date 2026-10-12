use log::{debug, trace};
use std::{
    env,
    path::{Path, PathBuf},
};

use crate::error::{Error, Result};

/// Finds the nearest ancestor of the current directory that contains `.git`.
///
/// # Errors
/// Fails if the current directory can't be read or no ancestor has `.git`.
pub fn find_git_root() -> Result<PathBuf> {
    find_git_root_from(&env::current_dir().map_err(Error::CurrentDir)?)
}

/// Finds the nearest ancestor of `start` (inclusive) that contains `.git`.
///
/// `.git` may be a directory or, in worktrees and submodules, a file.
///
/// # Errors
/// Fails if no ancestor of `start` has `.git`.
pub fn find_git_root_from(start: &Path) -> Result<PathBuf> {
    debug!("Searching for git root from {}", start.display());
    for dir in start.ancestors() {
        trace!("Checking for .git in {}", dir.display());
        if dir.join(".git").exists() {
            debug!("Found git root at {}", dir.display());
            return Ok(dir.to_path_buf());
        }
    }
    Err(Error::GitRootNotFound(start.to_path_buf()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    // These tests pass explicit paths instead of changing the process-wide
    // working directory, which raced with other tests running in parallel.

    #[test]
    fn test_find_git_root_from_subdir() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();
        fs::create_dir_all(root.join(".git")).unwrap();
        let subdir = root.join("src").join("components");
        fs::create_dir_all(&subdir).unwrap();

        assert_eq!(find_git_root_from(&subdir).unwrap(), root);
    }

    #[test]
    fn test_find_git_root_from_worktree_file() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();
        fs::write(root.join(".git"), "gitdir: /elsewhere").unwrap();

        assert_eq!(find_git_root_from(root).unwrap(), root);
    }

    #[test]
    fn test_find_git_root_from_not_found() {
        let temp_dir = TempDir::new().unwrap();
        let subdir = temp_dir.path().join("nested").join("deep");
        fs::create_dir_all(&subdir).unwrap();

        // Only meaningful when the temp dir is not itself inside a git checkout.
        if temp_dir.path().ancestors().any(|d| d.join(".git").exists()) {
            return;
        }
        assert!(matches!(find_git_root_from(&subdir), Err(Error::GitRootNotFound(_))));
    }
}
