use anyhow::Result;
use dashmap::DashMap;
use log::{debug, trace};
use path_clean::clean;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use crate::constants::{INDEX_FILES, RESOLVE_EXTENSIONS};

pub fn resolve(
    root: &Path,
    tsconfig_paths: &HashMap<String, Vec<String>>,
    from_file: &Path,
    request: &str,
    cache: &DashMap<(PathBuf, String), Option<PathBuf>>,
) -> Result<Option<PathBuf>> {
    let key = (from_file.to_path_buf(), request.to_string());
    if let Some(v) = cache.get(&key) {
        trace!("Cache hit for resolve: '{}' from {}", request, from_file.display());
        return Ok(v.clone());
    }
    trace!("Resolving: '{}' from {}", request, from_file.display());

    let resolved =
        if request.starts_with("./") || request.starts_with("../") || request.starts_with("/") {
            // Relative imports
            trace!("Resolving as relative import: '{}'", request);
            let base = from_file.parent().unwrap_or(root);
            let p = clean(base.join(request).to_string_lossy().to_string());
            let result = resolve_file(Path::new(&p));
            if result.is_some() {
                trace!("Resolved relative import '{}' to {:?}", request, result);
            } else {
                trace!("Failed to resolve relative import '{}'", request);
            }
            result
        } else {
            // Check tsconfig path aliases first
            trace!("Checking tsconfig path aliases for '{}'", request);
            let mut alias_resolved = None;
            for (alias, targets) in tsconfig_paths {
                if request.starts_with(alias) {
                    trace!("Matched alias '{}' for request '{}'", alias, request);
                    // Replace alias with target path
                    let remainder = request.trim_start_matches(alias).trim_start_matches('/');
                    for target in targets {
                        let candidate = if remainder.is_empty() {
                            PathBuf::from(target)
                        } else {
                            PathBuf::from(target).join(remainder)
                        };
                        if let Some(resolved) = resolve_file(&candidate) {
                            trace!("Resolved alias '{}' to {:?}", alias, resolved);
                            alias_resolved = Some(resolved);
                            break;
                        }
                    }
                    if alias_resolved.is_some() {
                        break;
                    }
                }
            }

            if alias_resolved.is_some() {
                alias_resolved
            } else {
                // Fallback to node_modules resolution - start from the file's directory
                trace!("Resolving as node_modules package: '{}'", request);
                let start_dir = from_file.parent().unwrap_or(root);
                let result = resolve_node_module_from_dir(start_dir, request, root);
                if result.is_some() {
                    trace!("Resolved node_modules package '{}' to {:?}", request, result);
                } else {
                    trace!("Failed to resolve node_modules package '{}'", request);
                }
                result
            }
        };

    cache.insert(key, resolved.clone());
    if resolved.is_some() {
        debug!("Successfully resolved '{}' from {}", request, from_file.display());
    }
    Ok(resolved)
}

fn resolve_file(p: &Path) -> Option<PathBuf> {
    // Try exact path first
    if p.exists() {
        return Some(p.canonicalize().unwrap_or_else(|_| p.to_path_buf()));
    }

    // Try adding extensions
    for ext in RESOLVE_EXTENSIONS {
        let candidate = PathBuf::from(format!("{}.{}", p.display(), ext));
        if candidate.exists() {
            return Some(candidate.canonicalize().unwrap_or(candidate));
        }
    }

    // Try index files
    for index_file in INDEX_FILES {
        let candidate = p.join(index_file);
        if candidate.exists() {
            return Some(candidate.canonicalize().unwrap_or(candidate));
        }
    }

    None
}

fn resolve_node_module_from_dir(
    start_dir: &Path,
    pkg: &str,
    workspace_root: &Path,
) -> Option<PathBuf> {
    trace!("Walking up from {:?} to find node_modules for '{}'", start_dir, pkg);
    // Walk up the directory tree looking for node_modules
    let mut current_dir = start_dir;

    loop {
        let result = resolve_node_module(current_dir, pkg);
        if result.is_some() {
            return result;
        }

        // Stop at workspace root
        if current_dir == workspace_root {
            break;
        }

        // Move up one directory
        current_dir = current_dir.parent()?;
    }

    None
}

fn resolve_node_module(root: &Path, pkg: &str) -> Option<PathBuf> {
    // Handle scoped packages like @nominal-io/ui
    let nm = root.join("node_modules").join(pkg);
    if !nm.exists() {
        trace!("node_modules path does not exist: {:?}", nm);
        return None;
    }
    trace!("Checking node_modules at: {:?}", nm);

    let pkg_json = nm.join("package.json");
    if pkg_json.exists()
        && let Ok(txt) = fs::read_to_string(&pkg_json)
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt)
    {
        // Try exports field first (modern packages)
        if let Some(exports) = v.get("exports") {
            // Handle string exports
            if let Some(s) = exports.as_str() {
                let p = nm.join(s.trim_start_matches("./"));
                if let Some(resolved) = resolve_file(&p) {
                    return Some(resolved);
                }
            }
            // Handle object exports - look for "." or "./index" entry
            if let Some(obj) = exports.as_object() {
                // Try "." first (default export)
                if let Some(dot_export) = obj.get(".") {
                    if let Some(s) = dot_export.as_str() {
                        let p = nm.join(s.trim_start_matches("./"));
                        if let Some(resolved) = resolve_file(&p) {
                            return Some(resolved);
                        }
                    }
                    // Handle conditional exports like { ".": { "import": "./dist/index.js" } }
                    if let Some(conditions) = dot_export.as_object() {
                        // Prefer import, then require, then default
                        for key in ["import", "require", "default"] {
                            if let Some(s) = conditions.get(key).and_then(|x| x.as_str()) {
                                let p = nm.join(s.trim_start_matches("./"));
                                if let Some(resolved) = resolve_file(&p) {
                                    return Some(resolved);
                                }
                            }
                        }
                    }
                }
            }
        }

        // Try module field (ESM entry point)
        if let Some(s) = v.get("module").and_then(|x| x.as_str()) {
            let p = nm.join(s);
            if let Some(resolved) = resolve_file(&p) {
                return Some(resolved);
            }
        }

        // Try main field
        if let Some(s) = v.get("main").and_then(|x| x.as_str()) {
            let p = nm.join(s);
            if let Some(resolved) = resolve_file(&p) {
                return Some(resolved);
            }
        }
    }

    // Fallback to common index files
    for index_file in INDEX_FILES {
        let p = nm.join(index_file);
        if p.exists() {
            return Some(p.canonicalize().unwrap_or(p));
        }
    }

    None
}
