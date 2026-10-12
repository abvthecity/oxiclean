use anyhow::{Result, anyhow};
use log::{debug, trace};
use std::{env, path::PathBuf};

pub fn find_git_root() -> Result<PathBuf> {
    debug!("Searching for git root");
    let mut current_dir = env::current_dir()?;
    trace!("Starting search from: {:?}", current_dir);

    loop {
        let git_dir = current_dir.join(".git");
        trace!("Checking for .git at: {:?}", git_dir);
        if git_dir.exists() {
            debug!("Found git root at: {:?}", current_dir);
            return Ok(current_dir);
        }

        // Try to move up to parent directory
        match current_dir.parent() {
            Some(parent) => current_dir = parent.to_path_buf(),
            None => {
                debug!("Could not find .git directory in any parent folder");
                return Err(anyhow!("Could not find .git directory in any parent folder"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_find_git_root() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        // Create .git directory
        fs::create_dir_all(root.join(".git")).unwrap();

        // Create a subdirectory
        let subdir = root.join("src").join("components");
        fs::create_dir_all(&subdir).unwrap();

        // Change to subdirectory
        let original_dir = env::current_dir().unwrap();
        env::set_current_dir(&subdir).unwrap();

        let git_root = find_git_root().unwrap();
        // Normalize paths for comparison (canonicalize can add /private prefix on macOS)
        assert_eq!(git_root.canonicalize().unwrap(), root.canonicalize().unwrap());

        // Restore original directory
        env::set_current_dir(original_dir).unwrap();
    }

    #[test]
    fn test_find_git_root_not_found() {
        let temp_dir = TempDir::new().unwrap();
        let subdir = temp_dir.path().join("nested").join("deep");
        fs::create_dir_all(&subdir).unwrap();

        // Don't create .git directory
        let original_dir = env::current_dir().unwrap();
        env::set_current_dir(&subdir).unwrap();

        let result = find_git_root();
        assert!(result.is_err());

        // Restore original directory
        env::set_current_dir(original_dir).unwrap();
    }
}
