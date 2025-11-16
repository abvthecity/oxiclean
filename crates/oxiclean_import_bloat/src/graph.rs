use anyhow::Result;
use dashmap::DashMap;
use log::{debug, trace};
use std::{collections::HashSet, path::PathBuf};

use crate::{config::Config, parser::imports_for, resolver::resolve, types::Specifier};

pub(crate) fn reachable_modules(
    cfg: &Config,
    start: &PathBuf,
    import_cache: &DashMap<PathBuf, Vec<Specifier>>,
    resolve_cache: &DashMap<(PathBuf, String), Option<PathBuf>>,
    reachable_cache: &DashMap<PathBuf, HashSet<PathBuf>>,
) -> Result<HashSet<PathBuf>> {
    if let Some(cached) = reachable_cache.get(start) {
        trace!("Cache hit for reachable modules: {}", start.display());
        return Ok(cached.clone());
    }
    trace!("Computing reachable modules from: {}", start.display());
    let mut visited: HashSet<PathBuf> = HashSet::new();
    let mut stack: Vec<PathBuf> = vec![start.clone()];

    while let Some(cur) = stack.pop() {
        if visited.contains(&cur) {
            continue;
        }
        visited.insert(cur.clone());
        trace!("Visiting module: {}", cur.display());

        let specs = imports_for(&cur, import_cache).unwrap_or_default();
        trace!("Module has {} imports", specs.len());

        for s in specs {
            if let Some(next) = resolve(cfg, &cur, &s.request, resolve_cache)?
                && !visited.contains(&next)
            {
                trace!("Adding to stack: {}", next.display());
                stack.push(next);
            }
        }
    }

    debug!("Computed {} reachable modules from {}", visited.len(), start.display());
    reachable_cache.insert(start.clone(), visited.clone());
    Ok(visited)
}
