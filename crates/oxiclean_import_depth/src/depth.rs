use anyhow::Result;
use dashmap::DashMap;
use log::{debug, trace, warn};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

use oxiclean_core::{imports_for, resolve};

/// Computes the maximum depth of the import tree starting from a given file.
///
/// This function performs a depth-first search through the import graph,
/// tracking the maximum depth encountered. It uses memoization to avoid
/// recomputing depths for files that have already been analyzed.
///
/// # Arguments
/// * `root` - The root directory of the project
/// * `tsconfig_paths` - TypeScript path mappings from tsconfig.json
/// * `start` - The file to start the depth analysis from
/// * `import_cache` - Cache of parsed imports for each file
/// * `resolve_cache` - Cache of resolved import paths
/// * `depth_cache` - Cache of computed depths for each file
///
/// # Returns
/// The maximum depth of imports from the starting file
pub fn compute_depth(
    root: &Path,
    tsconfig_paths: &HashMap<String, Vec<String>>,
    start: &Path,
    import_cache: &DashMap<PathBuf, Vec<oxiclean_core::Specifier>>,
    resolve_cache: &DashMap<(PathBuf, String), Option<PathBuf>>,
    depth_cache: &DashMap<PathBuf, usize>,
) -> Result<usize> {
    let mut visiting = HashSet::new();
    compute_depth_internal(
        root,
        tsconfig_paths,
        start,
        import_cache,
        resolve_cache,
        depth_cache,
        &mut visiting,
    )
}

/// Internal depth computation with cycle detection
fn compute_depth_internal(
    root: &Path,
    tsconfig_paths: &HashMap<String, Vec<String>>,
    start: &Path,
    import_cache: &DashMap<PathBuf, Vec<oxiclean_core::Specifier>>,
    resolve_cache: &DashMap<(PathBuf, String), Option<PathBuf>>,
    depth_cache: &DashMap<PathBuf, usize>,
    visiting: &mut HashSet<PathBuf>,
) -> Result<usize> {
    if let Some(cached) = depth_cache.get(start) {
        trace!("Cache hit for depth: {}", start.display());
        return Ok(*cached);
    }

    // Detect cycles - if we're already visiting this file, return 0 to break the cycle
    if visiting.contains(start) {
        trace!("Cycle detected at: {}", start.display());
        return Ok(0);
    }

    trace!("Computing depth from: {}", start.display());

    // Mark this file as being visited
    visiting.insert(start.to_path_buf());

    // Get imports for this file
    let specs = match imports_for(start, import_cache) {
        Ok(specs) => specs,
        Err(e) => {
            warn!("Error parsing imports for {}: {}", start.display(), e);
            visiting.remove(start);
            depth_cache.insert(start.to_path_buf(), 0);
            return Ok(0);
        }
    };

    if specs.is_empty() {
        trace!("No imports found in {}", start.display());
        visiting.remove(start);
        depth_cache.insert(start.to_path_buf(), 0);
        return Ok(0);
    }

    let mut max_depth = 0;

    for spec in specs {
        trace!("Checking import: '{}'", spec.request);

        let resolved = match resolve(root, tsconfig_paths, start, &spec.request, resolve_cache) {
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

        // Recursively compute depth for the resolved import
        let child_depth = compute_depth_internal(
            root,
            tsconfig_paths,
            &resolved,
            import_cache,
            resolve_cache,
            depth_cache,
            visiting,
        )?;

        // The depth through this import is 1 + the child's depth
        let depth_through_import = 1 + child_depth;
        if depth_through_import > max_depth {
            max_depth = depth_through_import;
        }
    }

    // Remove from visiting set before returning
    visiting.remove(start);

    debug!("Computed depth {} from {}", max_depth, start.display());
    depth_cache.insert(start.to_path_buf(), max_depth);
    Ok(max_depth)
}

/// Computes the depth for each direct import from a file.
///
/// This function returns a map of import specifiers to their depths,
/// allowing the caller to identify which specific imports have excessive depth.
///
/// # Returns
/// A vector of tuples containing (import_request, resolved_path, depth)
pub fn compute_import_depths(
    root: &Path,
    tsconfig_paths: &HashMap<String, Vec<String>>,
    from_file: &Path,
    import_cache: &DashMap<PathBuf, Vec<oxiclean_core::Specifier>>,
    resolve_cache: &DashMap<(PathBuf, String), Option<PathBuf>>,
    depth_cache: &DashMap<PathBuf, usize>,
) -> Result<Vec<(String, Option<PathBuf>, usize)>> {
    trace!("Computing import depths from: {}", from_file.display());

    let specs = match imports_for(from_file, import_cache) {
        Ok(specs) => specs,
        Err(e) => {
            warn!("Error parsing imports for {}: {}", from_file.display(), e);
            return Ok(vec![]);
        }
    };

    let mut results = Vec::new();

    for spec in specs {
        trace!("Analyzing import: '{}'", spec.request);

        let resolved = match resolve(root, tsconfig_paths, from_file, &spec.request, resolve_cache)
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

        // Compute depth for this resolved import (uses cycle detection internally)
        let depth = compute_depth(
            root,
            tsconfig_paths,
            &resolved,
            import_cache,
            resolve_cache,
            depth_cache,
        )?;

        // The depth of importing this module is 1 + its internal depth
        let import_depth = 1 + depth;

        trace!(
            "Import '{}' resolved to {} has depth {}",
            spec.request,
            resolved.display(),
            import_depth
        );

        results.push((spec.request.clone(), Some(resolved), import_depth));
    }

    debug!("Computed {} import depths from {}", results.len(), from_file.display());
    Ok(results)
}
