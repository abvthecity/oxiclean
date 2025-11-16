use anyhow::Result;
use ignore::WalkBuilder;
use log::{debug, trace};
use std::{collections::HashMap, path::PathBuf};

use crate::constants::JS_TS_EXTENSIONS;

pub struct CollectorConfig {
    pub root: PathBuf,
    pub entry_glob: Option<String>,
    pub tsconfig_paths: HashMap<String, Vec<String>>,
}

pub fn collect_entries(cfg: &CollectorConfig) -> Result<Vec<PathBuf>> {
    debug!("Collecting entry files");
    // If entries glob provided, walk and filter by suffix; else treat all top-level src files as entries
    let mut files: Vec<PathBuf> = Vec::new();
    let root = &cfg.root;
    debug!("Walking directory tree from root: {}", root.display());
    let walker = WalkBuilder::new(root).hidden(false).ignore(true).git_ignore(true).build();

    for res in walker {
        let dent = res?;
        let p = dent.path();
        if !p.is_file() {
            continue;
        }

        // Skip test files (*.test.*, *.spec.*)
        let path_str = p.to_string_lossy();
        if path_str.contains(".test.") || path_str.contains(".spec.") {
            trace!("Skipping test file: {}", path_str);
            continue;
        }

        if let Some(ext) = p.extension().and_then(|e| e.to_str())
            && JS_TS_EXTENSIONS.contains(&ext)
        {
            // If entry_glob is set, check if the relative path from root contains the pattern
            if let Some(gl) = &cfg.entry_glob {
                if let Ok(rel_path) = p.strip_prefix(root) {
                    let rel_str = rel_path.to_string_lossy();
                    // Match if relative path contains the glob pattern
                    if rel_str.contains(gl) {
                        trace!("Matched entry file with glob '{}': {}", gl, rel_str);
                        files.push(p.to_path_buf());
                    }
                }
            } else {
                // Heuristic: anything under src is considered
                if p.to_string_lossy().contains("/src/") {
                    trace!("Found entry file in /src/: {}", p.display());
                    files.push(p.to_path_buf());
                }
            }
        }
    }
    debug!("Collected {} entry files", files.len());
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
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
    fn test_collect_entries_from_src() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        // Create files in src directory
        create_test_file(root, "src/file1.js", "// file1");
        create_test_file(root, "src/file2.ts", "// file2");
        create_test_file(root, "src/components/Button.tsx", "// button");

        // File outside src should not be collected
        create_test_file(root, "lib/utils.js", "// utils");

        let cfg = CollectorConfig {
            root: root.to_path_buf(),
            entry_glob: None,
            tsconfig_paths: HashMap::new(),
        };

        let entries = collect_entries(&cfg).unwrap();
        assert_eq!(entries.len(), 3);

        let entry_names: Vec<String> =
            entries.iter().map(|p| p.file_name().unwrap().to_string_lossy().to_string()).collect();
        assert!(entry_names.contains(&"file1.js".to_string()));
        assert!(entry_names.contains(&"file2.ts".to_string()));
        assert!(entry_names.contains(&"Button.tsx".to_string()));
    }

    #[test]
    fn test_collect_entries_with_glob() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        create_test_file(root, "src/index.js", "// index");
        create_test_file(root, "src/components/Button.js", "// button");
        create_test_file(root, "src/utils.js", "// utils");

        let cfg = CollectorConfig {
            root: root.to_path_buf(),
            entry_glob: Some("index".to_string()),
            tsconfig_paths: HashMap::new(),
        };

        let entries = collect_entries(&cfg).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].to_string_lossy().contains("index.js"));
    }

    #[test]
    fn test_collect_entries_skips_test_files() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        create_test_file(root, "src/file.js", "// file");
        create_test_file(root, "src/file.test.js", "// test");
        create_test_file(root, "src/file.spec.ts", "// spec");

        let cfg = CollectorConfig {
            root: root.to_path_buf(),
            entry_glob: None,
            tsconfig_paths: HashMap::new(),
        };

        let entries = collect_entries(&cfg).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].to_string_lossy().contains("file.js"));
        assert!(!entries[0].to_string_lossy().contains(".test."));
        assert!(!entries[0].to_string_lossy().contains(".spec."));
    }

    #[test]
    fn test_collect_entries_only_js_ts_files() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        create_test_file(root, "src/file.js", "// js");
        create_test_file(root, "src/file.ts", "// ts");
        create_test_file(root, "src/file.tsx", "// tsx");
        create_test_file(root, "src/file.jsx", "// jsx");
        create_test_file(root, "src/file.mjs", "// mjs");
        create_test_file(root, "src/file.cjs", "// cjs");
        create_test_file(root, "src/file.mts", "// mts");
        create_test_file(root, "src/file.cts", "// cts");

        // Non-JS/TS files should be skipped
        create_test_file(root, "src/file.json", "{}");
        create_test_file(root, "src/file.txt", "text");

        let cfg = CollectorConfig {
            root: root.to_path_buf(),
            entry_glob: None,
            tsconfig_paths: HashMap::new(),
        };

        let entries = collect_entries(&cfg).unwrap();
        assert_eq!(entries.len(), 8); // All JS/TS variants

        let extensions: Vec<String> =
            entries.iter().map(|p| p.extension().unwrap().to_string_lossy().to_string()).collect();
        assert!(!extensions.contains(&"json".to_string()));
        assert!(!extensions.contains(&"txt".to_string()));
    }

    #[test]
    fn test_collect_entries_empty_when_no_matches() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        create_test_file(root, "lib/utils.js", "// utils");

        let cfg = CollectorConfig {
            root: root.to_path_buf(),
            entry_glob: None,
            tsconfig_paths: HashMap::new(),
        };

        let entries = collect_entries(&cfg).unwrap();
        assert_eq!(entries.len(), 0);
    }

    #[test]
    fn test_collect_entries_with_glob_pattern() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        create_test_file(root, "src/pages/home.js", "// home");
        create_test_file(root, "src/pages/about.js", "// about");
        create_test_file(root, "src/components/Button.js", "// button");

        let cfg = CollectorConfig {
            root: root.to_path_buf(),
            entry_glob: Some("pages".to_string()),
            tsconfig_paths: HashMap::new(),
        };

        let entries = collect_entries(&cfg).unwrap();
        assert_eq!(entries.len(), 2);

        for entry in &entries {
            assert!(entry.to_string_lossy().contains("pages"));
        }
    }
}
