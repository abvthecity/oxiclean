use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use ignore::WalkBuilder;
use std::path::{Component, Path, PathBuf};
use tracing::{debug, trace};

use crate::{
    constants::JS_TS_EXTENSIONS,
    error::{Error, Result},
};

/// Where and how to look for entry files.
#[derive(Debug, Clone, Default)]
pub struct CollectorConfig {
    pub root: PathBuf,
    /// Patterns matched against each file's path relative to `root`; a file
    /// matching any of them is an entry. Patterns with glob syntax (`*`, `?`,
    /// `[`, `{`) are globs; plain strings match any path containing them, as
    /// `--entry-glob` always has. When empty, every JS/TS file inside a `src`
    /// directory is an entry.
    pub entry_globs: Vec<String>,
}

/// Directories whose files are tests or fixtures rather than application code.
const SKIPPED_DIRS: &[&str] = &["node_modules", "__tests__", "__mocks__", "__fixtures__"];

/// Collects entry files under `cfg.root`, sorted by path.
///
/// `.gitignore` files are honoured even when the root is not inside a git
/// repository. `node_modules`, test directories, `*.test.*`/`*.spec.*` files
/// and `.d.ts` declarations are always skipped.
///
/// # Errors
/// Fails if a glob is invalid or the directory walk fails.
pub fn collect_entries(cfg: &CollectorConfig) -> Result<Vec<PathBuf>> {
    let root = &cfg.root;
    let matcher = EntryMatcher::new(&cfg.entry_globs)?;
    debug!("Walking directory tree from root: {}", root.display());

    let walker = WalkBuilder::new(root)
        .hidden(false)
        .ignore(true)
        .git_ignore(true)
        // Without this, .gitignore is ignored outside a git repository and
        // node_modules-like output directories get walked.
        .require_git(false)
        .filter_entry(|e| {
            e.file_name().to_str().is_none_or(|n| n != ".git" && !SKIPPED_DIRS.contains(&n))
        })
        .build();

    let mut files = Vec::new();
    for res in walker {
        let dent = res.map_err(|source| Error::Walk { path: root.clone(), source })?;
        let p = dent.path();
        // `is_file` follows symlinks, so symlinked sources are still entries.
        if !p.is_file() {
            continue;
        }
        if !is_source_file(p) {
            trace!("Skipping non-source, test or declaration file: {}", p.display());
            continue;
        }
        let rel = p.strip_prefix(root).unwrap_or(p);
        let is_entry = match &matcher {
            Some(m) => m.is_match(rel),
            None => is_under_src(rel),
        };
        if is_entry {
            trace!("Found entry file: {}", rel.display());
            files.push(p.to_path_buf());
        }
    }
    files.sort();
    debug!("Collected {} entry files", files.len());
    Ok(files)
}

/// Every file under `root` outside `node_modules` and `.git`, sorted by
/// path, for the checks of exports to pick test and declaration files from.
/// `.gitignore` files are honoured as in [`collect_entries`].
///
/// # Errors
/// Fails if the directory walk fails.
pub(crate) fn collect_project_files(root: &Path) -> Result<Vec<PathBuf>> {
    let walker = WalkBuilder::new(root)
        .hidden(false)
        .require_git(false)
        .filter_entry(|e| e.file_name() != ".git" && e.file_name() != "node_modules")
        .build();
    let mut files = Vec::new();
    for res in walker {
        let dent = res.map_err(|source| Error::Walk { path: root.to_path_buf(), source })?;
        if dent.file_type().is_some_and(|t| t.is_file()) || dent.path().is_file() {
            files.push(dent.into_path());
        }
    }
    files.sort();
    Ok(files)
}

/// A test file, by its path relative to the root: `*.test.*` and `*.spec.*`
/// files and files in `__tests__` directories. Tests use a project's exports
/// as other code does.
pub(crate) fn is_test_file(rel: &Path) -> bool {
    let Some(name) = rel.file_name().and_then(|n| n.to_str()) else { return false };
    let Some(ext) = rel.extension().and_then(|e| e.to_str()) else { return false };
    if !JS_TS_EXTENSIONS.contains(&ext) || is_declaration_name(name) {
        return false;
    }
    let in_tests_dir = rel
        .parent()
        .into_iter()
        .flat_map(Path::components)
        .any(|c| c == Component::Normal("__tests__".as_ref()));
    in_tests_dir || name.contains(".test.") || name.contains(".spec.")
}

fn is_declaration_name(name: &str) -> bool {
    name.ends_with(".d.ts") || name.ends_with(".d.mts") || name.ends_with(".d.cts")
}

/// The `--entry-glob` filter: real globs plus plain substrings.
struct EntryMatcher {
    globs: GlobSet,
    substrings: Vec<String>,
}

impl EntryMatcher {
    fn new(patterns: &[String]) -> Result<Option<Self>> {
        if patterns.is_empty() {
            return Ok(None);
        }
        let mut builder = GlobSetBuilder::new();
        let mut substrings = Vec::new();
        for pattern in patterns {
            if !pattern.contains(['*', '?', '[', '{']) {
                substrings.push(pattern.clone());
                continue;
            }
            // `*` stops at `/`, like .gitignore; use `**` to cross directories.
            let glob = GlobBuilder::new(pattern.trim_start_matches("./"))
                .literal_separator(true)
                .build()
                .map_err(|source| Error::InvalidGlob { glob: pattern.clone(), source })?;
            builder.add(glob);
        }
        let globs = builder
            .build()
            .map_err(|source| Error::InvalidGlob { glob: patterns.join(", "), source })?;
        Ok(Some(Self { globs, substrings }))
    }

    fn is_match(&self, rel: &Path) -> bool {
        if self.globs.is_match(rel) {
            return true;
        }
        if self.substrings.is_empty() {
            return false;
        }
        let rel = rel.to_string_lossy().replace('\\', "/");
        self.substrings.iter().any(|s| rel.contains(s.as_str()))
    }
}

/// A JS/TS source file that is not a test or a type declaration.
fn is_source_file(p: &Path) -> bool {
    let Some(name) = p.file_name().and_then(|n| n.to_str()) else { return false };
    let Some(ext) = p.extension().and_then(|e| e.to_str()) else { return false };
    if !JS_TS_EXTENSIONS.contains(&ext) {
        return false;
    }
    let is_test = name.contains(".test.") || name.contains(".spec.");
    !is_declaration_name(name) && !is_test
}

/// Whether any parent directory of `rel` is named `src`. Compares whole path
/// components so it works with any separator.
fn is_under_src(rel: &Path) -> bool {
    let mut parents = rel.parent().into_iter().flat_map(Path::components);
    parents.any(|c| c == Component::Normal("src".as_ref()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
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
    fn test_collect_entries_from_src() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        // Create files in src directory
        create_test_file(root, "src/file1.js", "// file1");
        create_test_file(root, "src/file2.ts", "// file2");
        create_test_file(root, "src/components/Button.tsx", "// button");

        // File outside src should not be collected
        create_test_file(root, "lib/utils.js", "// utils");

        let cfg = CollectorConfig { root: root.to_path_buf(), entry_globs: vec![] };

        let entries = collect_entries(&cfg).unwrap();
        assert_eq!(entries.len(), 3);

        let entry_names: Vec<String> =
            entries.iter().map(|p| p.file_name().unwrap().to_string_lossy().to_string()).collect();
        assert!(entry_names.contains(&"file1.js".to_string()));
        assert!(entry_names.contains(&"file2.ts".to_string()));
        assert!(entry_names.contains(&"Button.tsx".to_string()));
    }

    #[test]
    fn test_collect_entries_with_glob() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        create_test_file(root, "src/index.js", "// index");
        create_test_file(root, "src/components/Button.js", "// button");
        create_test_file(root, "src/utils.js", "// utils");

        let cfg =
            CollectorConfig { root: root.to_path_buf(), entry_globs: vec!["index".to_string()] };

        let entries = collect_entries(&cfg).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].to_string_lossy().contains("index.js"));
    }

    #[test]
    fn test_collect_entries_skips_test_files() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        create_test_file(root, "src/file.js", "// file");
        create_test_file(root, "src/file.test.js", "// test");
        create_test_file(root, "src/file.spec.ts", "// spec");

        let cfg = CollectorConfig { root: root.to_path_buf(), entry_globs: vec![] };

        let entries = collect_entries(&cfg).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].to_string_lossy().contains("file.js"));
        assert!(!entries[0].to_string_lossy().contains(".test."));
        assert!(!entries[0].to_string_lossy().contains(".spec."));
    }

    #[test]
    fn test_collect_entries_only_js_ts_files() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        create_test_file(root, "src/file.js", "// js");
        create_test_file(root, "src/file.ts", "// ts");
        create_test_file(root, "src/file.tsx", "// tsx");
        create_test_file(root, "src/file.jsx", "// jsx");
        create_test_file(root, "src/file.mjs", "// mjs");
        create_test_file(root, "src/file.cjs", "// cjs");
        create_test_file(root, "src/file.mts", "// mts");
        create_test_file(root, "src/file.cts", "// cts");

        // Non-JS/TS files should be skipped
        create_test_file(root, "src/file.json", "{}");
        create_test_file(root, "src/file.txt", "text");

        let cfg = CollectorConfig { root: root.to_path_buf(), entry_globs: vec![] };

        let entries = collect_entries(&cfg).unwrap();
        assert_eq!(entries.len(), 8); // All JS/TS variants

        let extensions: Vec<String> =
            entries.iter().map(|p| p.extension().unwrap().to_string_lossy().to_string()).collect();
        assert!(!extensions.contains(&"json".to_string()));
        assert!(!extensions.contains(&"txt".to_string()));
    }

    #[test]
    fn test_collect_entries_empty_when_no_matches() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        create_test_file(root, "lib/utils.js", "// utils");

        let cfg = CollectorConfig { root: root.to_path_buf(), entry_globs: vec![] };

        let entries = collect_entries(&cfg).unwrap();
        assert_eq!(entries.len(), 0);
    }

    #[test]
    fn test_collect_entries_with_glob_pattern() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        create_test_file(root, "src/pages/home.js", "// home");
        create_test_file(root, "src/pages/about.js", "// about");
        create_test_file(root, "src/components/Button.js", "// button");

        let cfg = CollectorConfig {
            root: root.to_path_buf(),
            entry_globs: vec!["src/pages/*.js".to_string()],
        };

        let entries = collect_entries(&cfg).unwrap();
        assert_eq!(entries.len(), 2);

        for entry in &entries {
            assert!(entry.to_string_lossy().contains("pages"));
        }
    }

    #[test]
    fn test_collect_entries_skips_tests_mocks_and_declarations() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        create_test_file(root, "src/file.ts", "// file");
        create_test_file(root, "src/__tests__/file-test.ts", "// test");
        create_test_file(root, "src/__mocks__/fs.ts", "// mock");
        create_test_file(root, "src/types.d.ts", "// types");
        create_test_file(root, "node_modules/pkg/src/index.js", "// dep");

        let cfg = CollectorConfig { root: root.to_path_buf(), entry_globs: vec![] };

        let entries = collect_entries(&cfg).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].ends_with("src/file.ts"));
    }

    #[test]
    fn test_collect_entries_skips_node_modules_without_git() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();
        create_test_file(root, "src/a.ts", "");
        create_test_file(root, "packages/x/node_modules/pkg/src/c.js", "");

        let cfg = CollectorConfig { root: root.to_path_buf(), entry_globs: vec![] };
        assert_eq!(collect_entries(&cfg).unwrap(), vec![root.join("src/a.ts")]);
    }

    #[test]
    fn test_collect_entries_honours_gitignore_without_git() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();
        create_test_file(root, ".gitignore", "dist/\n");
        create_test_file(root, "src/a.ts", "");
        create_test_file(root, "dist/src/a.js", "");

        let cfg = CollectorConfig { root: root.to_path_buf(), entry_globs: vec![] };
        assert_eq!(collect_entries(&cfg).unwrap(), vec![root.join("src/a.ts")]);
    }

    #[test]
    fn test_collect_entries_real_glob() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();
        create_test_file(root, "src/pages/home.tsx", "");
        create_test_file(root, "src/pages/nested/about.tsx", "");
        create_test_file(root, "src/pages/util.ts", "");

        let star = CollectorConfig {
            root: root.to_path_buf(),
            entry_globs: vec!["src/pages/*.tsx".to_string()],
        };
        assert_eq!(collect_entries(&star).unwrap(), vec![root.join("src/pages/home.tsx")]);

        let globstar = CollectorConfig {
            root: root.to_path_buf(),
            entry_globs: vec!["src/pages/**/*.tsx".to_string()],
        };
        assert_eq!(
            collect_entries(&globstar).unwrap(),
            vec![root.join("src/pages/home.tsx"), root.join("src/pages/nested/about.tsx")]
        );
    }

    #[test]
    fn test_collect_entries_several_globs() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();
        create_test_file(root, "apps/web/main.ts", "");
        create_test_file(root, "apps/api/server.ts", "");
        create_test_file(root, "apps/api/lib.ts", "");

        let cfg = CollectorConfig {
            root: root.to_path_buf(),
            entry_globs: vec!["apps/web/main.ts".to_string(), "apps/*/server.ts".to_string()],
        };
        assert_eq!(
            collect_entries(&cfg).unwrap(),
            vec![root.join("apps/api/server.ts"), root.join("apps/web/main.ts")]
        );
    }

    #[test]
    fn test_collect_entries_plain_string_is_substring() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();
        create_test_file(root, "apps/web/pages/home.ts", "");
        create_test_file(root, "apps/web/lib/util.ts", "");

        let cfg =
            CollectorConfig { root: root.to_path_buf(), entry_globs: vec!["pages".to_string()] };
        assert_eq!(collect_entries(&cfg).unwrap(), vec![root.join("apps/web/pages/home.ts")]);
    }

    #[test]
    fn test_collect_entries_invalid_glob() {
        let temp_dir = TempDir::new().unwrap();
        let cfg = CollectorConfig {
            root: temp_dir.path().to_path_buf(),
            entry_globs: vec!["src/[".to_string()],
        };
        assert!(matches!(collect_entries(&cfg), Err(Error::InvalidGlob { .. })));
    }
}
