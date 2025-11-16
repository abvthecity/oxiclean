use anyhow::{Result, anyhow};
use dashmap::DashMap;
use log::{debug, info, trace, warn};
use rayon::prelude::*;
use std::{path::PathBuf, sync::Arc, thread};

use oxiclean_core::{CollectorConfig, collect_entries};

use crate::{
    config::Config,
    depth::compute_import_depths,
    types::{CheckResult, Warning},
};

pub fn run_import_depth_check(mut cfg: Config) -> Result<CheckResult> {
    info!("Starting import depth check");

    // Initialize config (resolve root, load tsconfig paths)
    cfg.initialize()?;
    let root = cfg.root()?.clone();

    debug!("Collecting entry files with glob: {:?}", cfg.entry_glob);
    let collector_cfg = CollectorConfig {
        root: root.clone(),
        entry_glob: cfg.entry_glob.clone(),
        tsconfig_paths: cfg.tsconfig_paths.clone(),
    };

    let entries = collect_entries(&collector_cfg)?;
    if entries.is_empty() {
        warn!("No entry files found under {}", root.display());
        return Err(anyhow!("No entry files found under {}", root.display()));
    }
    info!("Found {} entry files", entries.len());

    // Thread-safe caches using DashMap
    let import_cache: Arc<DashMap<PathBuf, Vec<oxiclean_core::Specifier>>> =
        Arc::new(DashMap::new());
    let resolve_cache: Arc<DashMap<(PathBuf, String), Option<PathBuf>>> = Arc::new(DashMap::new());
    let depth_cache: Arc<DashMap<PathBuf, usize>> = Arc::new(DashMap::new());

    // Wrap config in Arc for sharing across threads
    let cfg = Arc::new(cfg);

    info!("Processing {} entry files in parallel", entries.len());

    // Process entries in parallel using rayon
    let warnings: Vec<Warning> = entries
        .par_iter()
        .flat_map(|entry| {
            let thread_id = thread::current().id();
            debug!("Thread {:?} processing: {}", thread_id, entry.display());
            trace!("Computing import depths for entry: {}", entry.display());

            let cfg = Arc::clone(&cfg);
            let import_cache = Arc::clone(&import_cache);
            let resolve_cache = Arc::clone(&resolve_cache);
            let depth_cache = Arc::clone(&depth_cache);

            let root = match cfg.root() {
                Ok(r) => r.clone(),
                Err(e) => {
                    warn!("Error getting root: {}", e);
                    return vec![];
                }
            };

            // Get relative path for better display
            let rel_entry =
                entry.strip_prefix(&root).unwrap_or(entry).to_string_lossy().to_string();

            let mut entry_warnings = Vec::new();

            // Compute depths for each direct import from this entry
            trace!("Analyzing direct imports from entry");
            let import_depths = match compute_import_depths(
                &root,
                &cfg.tsconfig_paths,
                entry,
                &import_cache,
                &resolve_cache,
                &depth_cache,
            ) {
                Ok(depths) => depths,
                Err(e) => {
                    warn!("Error computing import depths for {}: {}", entry.display(), e);
                    return vec![];
                }
            };

            debug!("Entry has {} direct imports", import_depths.len());

            for (import_request, resolved_path, depth) in import_depths {
                trace!("Import '{}' has depth {}", import_request, depth);

                if depth >= cfg.threshold {
                    // Get the resolved path relative to root for display
                    let resolved_rel = resolved_path
                        .as_ref()
                        .and_then(|p| p.strip_prefix(&root).ok())
                        .map(|p| p.to_string_lossy().to_string());

                    entry_warnings.push(Warning {
                        import_statement: format!("import '{}'", import_request),
                        from_file: rel_entry.clone(),
                        depth,
                        resolved_path: resolved_rel,
                    });
                }
            }

            entry_warnings
        })
        .collect();

    info!("Import depth check complete. Found {} warnings", warnings.len());
    debug!(
        "Cache statistics: imports={}, resolutions={}, depths={}",
        import_cache.len(),
        resolve_cache.len(),
        depth_cache.len()
    );

    Ok(CheckResult { warnings, files_analyzed: import_cache.len() })
}
