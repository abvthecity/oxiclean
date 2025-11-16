use anyhow::Result;
use dashmap::DashMap;
use log::{debug, trace};
use std::{
    collections::HashMap,
    collections::HashSet,
    path::{Path, PathBuf},
};

use oxiclean_core::{Specifier, imports_for, resolve};

pub(crate) fn reachable_modules(
    root: &Path,
    tsconfig_paths: &HashMap<String, Vec<String>>,
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
            if let Some(next) = resolve(root, tsconfig_paths, &cur, &s.request, resolve_cache)?
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
    fn test_reachable_modules_simple() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        let entry = create_test_file(root, "src/index.js", "import './a'; import './b';");
        let a = create_test_file(root, "src/a.js", "// a");
        let b = create_test_file(root, "src/b.js", "// b");

        let import_cache = DashMap::new();
        let resolve_cache = DashMap::new();
        let reachable_cache = DashMap::new();

        let reachable = reachable_modules(
            root,
            &HashMap::new(),
            &entry,
            &import_cache,
            &resolve_cache,
            &reachable_cache,
        )
        .unwrap();

        assert_eq!(reachable.len(), 3); // entry, a, b
        // Normalize paths for comparison (canonicalize can add /private prefix on macOS)
        let reachable_canonical: std::collections::HashSet<_> =
            reachable.iter().map(|p| p.canonicalize().unwrap_or_else(|_| p.clone())).collect();
        assert!(
            reachable_canonical.contains(&entry.canonicalize().unwrap_or_else(|_| entry.clone()))
        );
        assert!(reachable_canonical.contains(&a.canonicalize().unwrap_or_else(|_| a.clone())));
        assert!(reachable_canonical.contains(&b.canonicalize().unwrap_or_else(|_| b.clone())));
    }

    #[test]
    fn test_reachable_modules_nested() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        let entry = create_test_file(root, "src/index.js", "import './a';");
        let a = create_test_file(root, "src/a.js", "import './b';");
        let b = create_test_file(root, "src/b.js", "import './c';");
        let c = create_test_file(root, "src/c.js", "// c");

        let import_cache = DashMap::new();
        let resolve_cache = DashMap::new();
        let reachable_cache = DashMap::new();

        let reachable = reachable_modules(
            root,
            &HashMap::new(),
            &entry,
            &import_cache,
            &resolve_cache,
            &reachable_cache,
        )
        .unwrap();

        assert_eq!(reachable.len(), 4); // entry, a, b, c
        // Normalize paths for comparison (canonicalize can add /private prefix on macOS)
        let reachable_canonical: std::collections::HashSet<_> =
            reachable.iter().map(|p| p.canonicalize().unwrap_or_else(|_| p.clone())).collect();
        assert!(
            reachable_canonical.contains(&entry.canonicalize().unwrap_or_else(|_| entry.clone()))
        );
        assert!(reachable_canonical.contains(&a.canonicalize().unwrap_or_else(|_| a.clone())));
        assert!(reachable_canonical.contains(&b.canonicalize().unwrap_or_else(|_| b.clone())));
        assert!(reachable_canonical.contains(&c.canonicalize().unwrap_or_else(|_| c.clone())));
    }

    #[test]
    fn test_reachable_modules_circular() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        let entry = create_test_file(root, "src/index.js", "import './a';");
        let a = create_test_file(root, "src/a.js", "import './b';");
        let b = create_test_file(root, "src/b.js", "import './a';"); // circular

        let import_cache = DashMap::new();
        let resolve_cache = DashMap::new();
        let reachable_cache = DashMap::new();

        let reachable = reachable_modules(
            root,
            &HashMap::new(),
            &entry,
            &import_cache,
            &resolve_cache,
            &reachable_cache,
        )
        .unwrap();

        // Should handle circular dependencies without infinite loop
        assert_eq!(reachable.len(), 3); // entry, a, b
        // Normalize paths for comparison (canonicalize can add /private prefix on macOS)
        let reachable_canonical: std::collections::HashSet<_> =
            reachable.iter().map(|p| p.canonicalize().unwrap_or_else(|_| p.clone())).collect();
        assert!(
            reachable_canonical.contains(&entry.canonicalize().unwrap_or_else(|_| entry.clone()))
        );
        assert!(reachable_canonical.contains(&a.canonicalize().unwrap_or_else(|_| a.clone())));
        assert!(reachable_canonical.contains(&b.canonicalize().unwrap_or_else(|_| b.clone())));
    }

    #[test]
    fn test_reachable_modules_cache() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        let entry = create_test_file(root, "src/index.js", "import './a';");
        let _a = create_test_file(root, "src/a.js", "// a");

        let import_cache = DashMap::new();
        let resolve_cache = DashMap::new();
        let reachable_cache = DashMap::new();

        // First call
        let reachable1 = reachable_modules(
            root,
            &HashMap::new(),
            &entry,
            &import_cache,
            &resolve_cache,
            &reachable_cache,
        )
        .unwrap();

        // Second call should use cache
        let reachable2 = reachable_modules(
            root,
            &HashMap::new(),
            &entry,
            &import_cache,
            &resolve_cache,
            &reachable_cache,
        )
        .unwrap();

        assert_eq!(reachable1.len(), reachable2.len());
        assert_eq!(reachable_cache.len(), 1);
    }

    #[test]
    fn test_reachable_modules_no_imports() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        let entry = create_test_file(root, "src/index.js", "// no imports");

        let import_cache = DashMap::new();
        let resolve_cache = DashMap::new();
        let reachable_cache = DashMap::new();

        let reachable = reachable_modules(
            root,
            &HashMap::new(),
            &entry,
            &import_cache,
            &resolve_cache,
            &reachable_cache,
        )
        .unwrap();

        assert_eq!(reachable.len(), 1); // only the entry itself
        assert!(reachable.contains(&entry));
    }

    #[test]
    fn test_reachable_modules_multiple_paths() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        // Entry imports a and b, both import c
        let entry = create_test_file(root, "src/index.js", "import './a'; import './b';");
        let a = create_test_file(root, "src/a.js", "import './c';");
        let b = create_test_file(root, "src/b.js", "import './c';");
        let c = create_test_file(root, "src/c.js", "// c");

        let import_cache = DashMap::new();
        let resolve_cache = DashMap::new();
        let reachable_cache = DashMap::new();

        let reachable = reachable_modules(
            root,
            &HashMap::new(),
            &entry,
            &import_cache,
            &resolve_cache,
            &reachable_cache,
        )
        .unwrap();

        // Should only count c once (no duplicates)
        assert_eq!(reachable.len(), 4); // entry, a, b, c
        // Normalize paths for comparison (canonicalize can add /private prefix on macOS)
        let reachable_canonical: std::collections::HashSet<_> =
            reachable.iter().map(|p| p.canonicalize().unwrap_or_else(|_| p.clone())).collect();
        assert!(
            reachable_canonical.contains(&entry.canonicalize().unwrap_or_else(|_| entry.clone()))
        );
        assert!(reachable_canonical.contains(&a.canonicalize().unwrap_or_else(|_| a.clone())));
        assert!(reachable_canonical.contains(&b.canonicalize().unwrap_or_else(|_| b.clone())));
        assert!(reachable_canonical.contains(&c.canonicalize().unwrap_or_else(|_| c.clone())));
    }
}
