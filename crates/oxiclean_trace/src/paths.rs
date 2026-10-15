//! Paths in a trace, found on this machine.
//!
//! A trace names files by the absolute paths `tsc` saw where it ran, which
//! may be another machine, a CI container or a checkout that has since
//! moved; on case-insensitive systems `tsc` also lowercases them. Each
//! path is looked up as written, then relative to the project root, then
//! by ever shorter tails under the root (`/ci/work/repo/src/a.ts` is
//! tried as `ci/work/repo/src/a.ts`, `work/repo/src/a.ts`, … `src/a.ts`),
//! matching each directory's entries without regard to case when the
//! exact name isn't there.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
};

/// Finds the files a trace names, remembering each answer.
#[derive(Debug)]
pub(crate) struct PathMap {
    root: PathBuf,
    cache: Mutex<HashMap<String, Option<PathBuf>>>,
}

impl PathMap {
    pub(crate) fn new(root: &Path) -> Self {
        // `root` is canonical, as the workspace's is.
        Self { root: root.to_path_buf(), cache: Mutex::new(HashMap::new()) }
    }

    /// The file or directory `raw`, as a trace writes it, on this machine;
    /// `None` when it can't be found.
    pub(crate) fn local(&self, raw: &str) -> Option<PathBuf> {
        if let Some(hit) = self.cache.lock().ok().and_then(|c| c.get(raw).cloned()) {
            return hit;
        }
        let found = self.find(raw);
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(raw.to_string(), found.clone());
        }
        found
    }

    fn find(&self, raw: &str) -> Option<PathBuf> {
        if raw.is_empty() {
            return None;
        }
        let as_written = Path::new(raw);
        if as_written.is_absolute() && as_written.exists() {
            return Some(as_written.to_path_buf());
        }
        // Windows paths (`C:/repo/a.ts`, `C:\repo\a.ts`) on any platform.
        let parts: Vec<&str> =
            raw.split(['/', '\\']).filter(|p| !p.is_empty() && *p != ".").collect();
        let first = parts.first().is_some_and(|p| p.ends_with(':')).into();
        for skip in first..parts.len() {
            if let Some(found) = under(&self.root, &parts[skip..]) {
                return Some(found);
            }
        }
        None
    }
}

/// `parts` joined under `dir`, each found exactly or else without regard
/// to case.
fn under(dir: &Path, parts: &[&str]) -> Option<PathBuf> {
    if parts.contains(&"..") {
        return None;
    }
    let exact = parts.iter().fold(dir.to_path_buf(), |p, part| p.join(part));
    if exact.exists() {
        return Some(exact);
    }
    let mut current = dir.to_path_buf();
    for part in parts {
        let next = current.join(part);
        current = if next.exists() {
            next
        } else {
            let wanted = part.to_lowercase();
            fs::read_dir(&current)
                .ok()?
                .flatten()
                .find(|e| e.file_name().to_string_lossy().to_lowercase() == wanted)?
                .path()
        };
    }
    Some(current)
}

/// `raw` with `/` separators, as analyze-trace's patterns expect.
pub(crate) fn slashes(raw: &str) -> String {
    raw.replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn finds_files_recorded_elsewhere() {
        let t = TempDir::new().unwrap();
        let root = t.path().canonicalize().unwrap();
        fs::create_dir_all(root.join("packages/ui/src")).unwrap();
        fs::write(root.join("packages/ui/src/Button.tsx"), "").unwrap();
        let map = PathMap::new(&root);
        let file = root.join("packages/ui/src/Button.tsx");

        let here = file.to_string_lossy().to_string();
        assert_eq!(map.local(&here), Some(file.clone()));
        assert_eq!(map.local("/ci/work/repo/packages/ui/src/Button.tsx"), Some(file.clone()));
        assert_eq!(map.local("packages/ui/src/Button.tsx"), Some(file.clone()));
        // Windows, lowercased as tsc writes paths there.
        assert_eq!(map.local("c:/users/me/repo/packages/ui/src/button.tsx"), Some(file.clone()));
        assert_eq!(map.local("C:\\Users\\me\\repo\\packages\\ui\\src\\Button.tsx"), Some(file));
        assert_eq!(map.local("/elsewhere/missing.ts"), None);
        assert_eq!(map.local(""), None);
        assert_eq!(map.local("/x/../../etc/passwd"), None);
    }
}
