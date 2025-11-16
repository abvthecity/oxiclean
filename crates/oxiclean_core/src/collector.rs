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
