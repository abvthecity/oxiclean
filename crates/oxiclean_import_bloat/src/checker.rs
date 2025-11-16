use anyhow::{Result, anyhow};
use dashmap::DashMap;
use log::{debug, info, trace, warn};
use rayon::prelude::*;
use std::{collections::HashSet, path::PathBuf, sync::Arc, thread};

use crate::{
    collector::collect_entries,
    config::{Config, find_git_root, read_tsconfig_paths},
    graph::reachable_modules,
    parser::imports_for,
    resolver::resolve,
    types::{CheckResult, Specifier, Warning},
};

pub fn run_import_bloat_check(mut cfg: Config) -> Result<CheckResult> {
    info!("Starting import bloat check");

    // Resolve root directory
    let root = if let Some(r) = cfg.root.take() {
        debug!("Using provided root directory: {:?}", r);
        r.canonicalize().unwrap_or(r)
    } else {
        debug!("No root provided, searching for git root");
        find_git_root()?
    };
    info!("Using root directory: {}", root.display());
    cfg.root = Some(root.clone());

    // Read tsconfig paths
    debug!("Reading tsconfig paths");
    cfg.tsconfig_paths = read_tsconfig_paths(&root);
    debug!("Found {} tsconfig path aliases", cfg.tsconfig_paths.len());

    debug!("Collecting entry files with glob: {:?}", cfg.entry_glob);
    let entries = collect_entries(&cfg)?;
    if entries.is_empty() {
        warn!("No entry files found under {}", cfg.root.as_ref().unwrap().display());
        return Err(anyhow!("No entry files found under {}", cfg.root.as_ref().unwrap().display()));
    }
    info!("Found {} entry files", entries.len());

    // Thread-safe caches using DashMap
    let import_cache: Arc<DashMap<PathBuf, Vec<Specifier>>> = Arc::new(DashMap::new());
    let resolve_cache: Arc<DashMap<(PathBuf, String), Option<PathBuf>>> = Arc::new(DashMap::new());
    let reachable_cache: Arc<DashMap<PathBuf, HashSet<PathBuf>>> = Arc::new(DashMap::new());

    // Wrap config in Arc for sharing across threads
    let cfg = Arc::new(cfg);

    info!("Processing {} entry files in parallel", entries.len());

    // Process entries in parallel using rayon
    let warnings: Vec<Warning> = entries
        .par_iter()
        .flat_map(|entry| {
            let thread_id = thread::current().id();
            debug!("Thread {:?} processing: {}", thread_id, entry.display());
            trace!("Computing reachable modules for entry: {}", entry.display());

            let cfg = Arc::clone(&cfg);
            let import_cache = Arc::clone(&import_cache);
            let resolve_cache = Arc::clone(&resolve_cache);
            let reachable_cache = Arc::clone(&reachable_cache);

            // Compute reachable modules for this entry
            let reachable = match reachable_modules(
                &cfg,
                entry,
                &import_cache,
                &resolve_cache,
                &reachable_cache,
            ) {
                Ok(r) => r,
                Err(e) => {
                    warn!("Error computing reachable modules for {}: {}", entry.display(), e);
                    return vec![];
                }
            };

            debug!("Entry {} has {} reachable modules", entry.display(), reachable.len());

            // Get relative path for better display
            let rel_entry = entry
                .strip_prefix(cfg.root.as_ref().unwrap())
                .unwrap_or(entry)
                .to_string_lossy()
                .to_string();

            let mut entry_warnings = Vec::new();

            // For each direct import from entry, compute its own reachable set and warn per-import
            trace!("Analyzing direct imports from entry");
            let direct_imports = match imports_for(entry, &import_cache) {
                Ok(imports) => imports,
                Err(e) => {
                    warn!("Error parsing imports for {}: {}", entry.display(), e);
                    return vec![];
                }
            };

            debug!("Entry has {} direct imports", direct_imports.len());

            for spec in direct_imports {
                trace!("Checking import: '{}'", spec.request);

                let resolved = match resolve(&cfg, entry, &spec.request, &resolve_cache) {
                    Ok(Some(r)) => r,
                    Ok(None) => {
                        trace!("Could not resolve import: '{}'", spec.request);
                        continue;
                    }
                    Err(e) => {
                        warn!("Error resolving '{}': {}", spec.request, e);
                        continue;
                    }
                };

                let rset = match reachable_modules(
                    &cfg,
                    &resolved,
                    &import_cache,
                    &resolve_cache,
                    &reachable_cache,
                ) {
                    Ok(r) => r,
                    Err(e) => {
                        warn!(
                            "Error computing reachable modules for {}: {}",
                            resolved.display(),
                            e
                        );
                        continue;
                    }
                };

                if rset.len() >= cfg.threshold {
                    // Get the resolved path relative to root for display
                    let resolved_rel = resolved
                        .strip_prefix(cfg.root.as_ref().unwrap())
                        .unwrap_or(&resolved)
                        .to_string_lossy()
                        .to_string();

                    entry_warnings.push(Warning {
                        import_statement: format!("import '{}'", spec.request),
                        from_file: rel_entry.clone(),
                        reachable_unique_modules: rset.len(),
                        resolved_path: Some(resolved_rel),
                    });
                }
            }

            // Also consider the whole entry's graph if desired
            if reachable.len() >= cfg.threshold {
                entry_warnings.push(Warning {
                    import_statement: "Entry file (entire graph)".to_string(),
                    from_file: rel_entry,
                    reachable_unique_modules: reachable.len(),
                    resolved_path: None,
                });
            }

            entry_warnings
        })
        .collect();

    info!("Import bloat check complete. Found {} warnings", warnings.len());
    debug!(
        "Cache statistics: imports={}, resolutions={}, reachable={}",
        import_cache.len(),
        resolve_cache.len(),
        reachable_cache.len()
    );

    Ok(CheckResult { warnings, files_analyzed: import_cache.len() })
}
