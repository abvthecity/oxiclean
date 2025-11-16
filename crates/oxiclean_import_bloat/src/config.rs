use anyhow::{Result, anyhow};
use clap::Parser;
use ignore::WalkBuilder;
use log::{debug, trace};
use std::{
    collections::HashMap,
    env, fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Parser)]
#[command(name = "import-bloat")]
#[command(about = "Check for import bloat in JavaScript/TypeScript projects")]
pub struct Config {
    /// Root directory of the project (defaults to git root)
    #[arg(long)]
    pub root: Option<PathBuf>,

    /// Threshold for number of modules
    #[arg(long, default_value = "200")]
    pub threshold: usize,

    /// Glob pattern to filter entry files
    #[arg(long)]
    pub entry_glob: Option<String>,

    #[clap(skip)]
    pub tsconfig_paths: HashMap<String, Vec<String>>,
}

pub(crate) fn find_git_root() -> Result<PathBuf> {
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

pub(crate) fn read_tsconfig_paths(root: &Path) -> HashMap<String, Vec<String>> {
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
