use anyhow::{Result, anyhow};
use ignore::WalkBuilder;
use log::{debug, trace};
use std::{
    collections::HashMap,
    env, fs,
    path::{Path, PathBuf},
};

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

pub fn read_tsconfig_paths(root: &Path) -> HashMap<String, Vec<String>> {
    debug!("Reading tsconfig paths from root: {:?}", root);
    let mut paths = HashMap::new();

    // Find all tsconfig.json files recursively
    let walker = WalkBuilder::new(root).hidden(false).git_ignore(true).build();

    let mut tsconfig_files = Vec::new();
    for entry in walker.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.file_name().and_then(|n| n.to_str()) == Some("tsconfig.json") {
            trace!("Found tsconfig at: {:?}", path);
            tsconfig_files.push(path.to_path_buf());
        }
    }

    debug!("Found {} tsconfig.json files", tsconfig_files.len());

    for tsconfig_path in &tsconfig_files {
        trace!("Checking tsconfig at: {:?}", tsconfig_path);
        if let Ok(content) = fs::read_to_string(tsconfig_path) {
            trace!("Found tsconfig at: {:?}", tsconfig_path);
            // Strip comments (simple approach - removes // comments)
            let content_no_comments: String = content
                .lines()
                .map(|line| if let Some(idx) = line.find("//") { &line[..idx] } else { line })
                .collect::<Vec<_>>()
                .join("\n");

            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content_no_comments)
                && let Some(compiler_options) = json.get("compilerOptions")
                && let Some(paths_obj) = compiler_options.get("paths").and_then(|p| p.as_object())
            {
                let base_url =
                    compiler_options.get("baseUrl").and_then(|b| b.as_str()).unwrap_or(".");

                let tsconfig_dir = tsconfig_path.parent().unwrap_or(root);
                let base_path = tsconfig_dir.join(base_url);

                for (alias, targets) in paths_obj {
                    if let Some(target_arr) = targets.as_array() {
                        let resolved_targets: Vec<String> = target_arr
                            .iter()
                            .filter_map(|t| t.as_str())
                            .map(|t| {
                                base_path
                                    .join(t.trim_end_matches("/*"))
                                    .to_string_lossy()
                                    .to_string()
                            })
                            .collect();

                        if !resolved_targets.is_empty() {
                            let alias_key = alias.trim_end_matches("/*").to_string();
                            trace!(
                                "Found tsconfig path alias: '{}' -> {:?}",
                                alias_key, resolved_targets
                            );
                            paths.insert(alias_key, resolved_targets);
                        }
                    }
                }
            }
        }
    }

    debug!("Loaded {} tsconfig path aliases", paths.len());
    paths
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_test_file(dir: &Path, path: &str, content: &str) -> PathBuf {
        let file_path = dir.join(path);
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent).expect("Failed to create parent directory");
        }
        fs::write(&file_path, content).expect("Failed to write test file");
        file_path
    }

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

    #[test]
    fn test_read_tsconfig_paths_simple() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        let tsconfig_content = r#"
{
  "compilerOptions": {
    "baseUrl": ".",
    "paths": {
      "@components/*": ["src/components/*"],
      "@utils": ["src/utils"]
    }
  }
}
"#;
        create_test_file(root, "tsconfig.json", tsconfig_content);
        create_test_file(root, "src/components/Button.tsx", "// button");
        create_test_file(root, "src/utils/index.ts", "// utils");

        let paths = read_tsconfig_paths(root);
        assert_eq!(paths.len(), 2);
        assert!(paths.contains_key("@components"));
        assert!(paths.contains_key("@utils"));

        let components_paths = paths.get("@components").unwrap();
        assert_eq!(components_paths.len(), 1);
        assert!(components_paths[0].contains("src/components"));
    }

    #[test]
    fn test_read_tsconfig_paths_with_base_url() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        let tsconfig_content = r#"
{
  "compilerOptions": {
    "baseUrl": "src",
    "paths": {
      "@components/*": ["components/*"]
    }
  }
}
"#;
        create_test_file(root, "tsconfig.json", tsconfig_content);

        let paths = read_tsconfig_paths(root);
        assert_eq!(paths.len(), 1);
        assert!(paths.contains_key("@components"));

        let components_paths = paths.get("@components").unwrap();
        assert!(components_paths[0].contains("src/components"));
    }

    #[test]
    fn test_read_tsconfig_paths_multiple_files() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        let root_tsconfig = r#"
{
  "compilerOptions": {
    "paths": {
      "@root/*": ["src/*"]
    }
  }
}
"#;
        let app_tsconfig = r#"
{
  "compilerOptions": {
    "paths": {
      "@app/*": ["app/*"]
    }
  }
}
"#;
        create_test_file(root, "tsconfig.json", root_tsconfig);
        create_test_file(root, "apps/web/tsconfig.json", app_tsconfig);

        let paths = read_tsconfig_paths(root);
        assert_eq!(paths.len(), 2);
        assert!(paths.contains_key("@root"));
        assert!(paths.contains_key("@app"));
    }

    #[test]
    fn test_read_tsconfig_paths_with_comments() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        let tsconfig_content = r#"
{
  // This is a comment
  "compilerOptions": {
    "baseUrl": ".", // Another comment
    "paths": {
      "@components/*": ["src/components/*"] // Path comment
    }
  }
}
"#;
        create_test_file(root, "tsconfig.json", tsconfig_content);

        let paths = read_tsconfig_paths(root);
        assert_eq!(paths.len(), 1);
        assert!(paths.contains_key("@components"));
    }

    #[test]
    fn test_read_tsconfig_paths_no_paths() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        let tsconfig_content = r#"
{
  "compilerOptions": {
    "target": "ES2020"
  }
}
"#;
        create_test_file(root, "tsconfig.json", tsconfig_content);

        let paths = read_tsconfig_paths(root);
        assert_eq!(paths.len(), 0);
    }

    #[test]
    fn test_read_tsconfig_paths_empty() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        // No tsconfig.json file
        let paths = read_tsconfig_paths(root);
        assert_eq!(paths.len(), 0);
    }

    #[test]
    fn test_read_tsconfig_paths_strips_trailing_slash() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        let tsconfig_content = r#"
{
  "compilerOptions": {
    "paths": {
      "@components/*": ["src/components/*"]
    }
  }
}
"#;
        create_test_file(root, "tsconfig.json", tsconfig_content);

        let paths = read_tsconfig_paths(root);
        // Should strip /* from alias
        assert!(paths.contains_key("@components"));
        assert!(!paths.contains_key("@components/*"));
    }
}
