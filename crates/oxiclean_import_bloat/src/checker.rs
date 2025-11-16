use anyhow::{Result, anyhow};
use dashmap::DashMap;
use log::{debug, info, trace, warn};
use rayon::prelude::*;
use std::{collections::HashSet, path::PathBuf, sync::Arc, thread};

use oxiclean_core::{CollectorConfig, Specifier, collect_entries, imports_for, resolve};

use crate::{
    config::Config,
    graph::reachable_modules,
    types::{CheckResult, Warning},
};

pub fn run_import_bloat_check(mut cfg: Config) -> Result<CheckResult> {
    info!("Starting import bloat check");

    // Initialize config (resolve root, load tsconfig paths)
    cfg.initialize()?;
    let root = cfg.root().ok_or_else(|| anyhow!("Config not initialized"))?.clone();

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

            let root = match cfg.root() {
                Some(r) => r.clone(),
                None => {
                    warn!("Config root not initialized");
                    return vec![];
                }
            };

            // Compute reachable modules for this entry
            let reachable = match reachable_modules(
                &root,
                &cfg.tsconfig_paths,
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
            let rel_entry =
                entry.strip_prefix(&root).unwrap_or(entry).to_string_lossy().to_string();

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

                let resolved =
                    match resolve(&root, &cfg.tsconfig_paths, entry, &spec.request, &resolve_cache)
                    {
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
                    &root,
                    &cfg.tsconfig_paths,
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
                        .strip_prefix(&root)
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
