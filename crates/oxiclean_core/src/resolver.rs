use anyhow::Result;
use dashmap::DashMap;
use std::{
    fs,
    path::{Path, PathBuf},
};
use tracing::{debug, trace};

use crate::constants::{
    DECLARATION_SUFFIXES, INDEX_FILES, JS_TS_EXTENSIONS, RESOLVE_EXTENSIONS, is_declaration,
};
use crate::project::{Project, normalize, split_package_request};

/// Node.js built-in modules, which never contribute to an app's module graph.
const NODE_BUILTINS: &[&str] = &[
    "assert",
    "async_hooks",
    "buffer",
    "child_process",
    "cluster",
    "console",
    "constants",
    "crypto",
    "dgram",
    "diagnostics_channel",
    "dns",
    "domain",
    "events",
    "fs",
    "http",
    "http2",
    "https",
    "inspector",
    "module",
    "net",
    "os",
    "path",
    "perf_hooks",
    "process",
    "punycode",
    "querystring",
    "readline",
    "repl",
    "stream",
    "string_decoder",
    "sys",
    "timers",
    "tls",
    "trace_events",
    "tty",
    "url",
    "util",
    "v8",
    "vm",
    "wasi",
    "worker_threads",
    "zlib",
];

/// Whether `request` names a runtime built-in (`fs`, `node:path`, `bun:test`, ...).
pub fn is_builtin(request: &str) -> bool {
    if request.starts_with("node:") || request.starts_with("bun:") {
        return true;
    }
    let (name, _) = split_package_request(request);
    NODE_BUILTINS.contains(&name)
}

/// How many distinct (file, import) pairs resolved, failed, or were built-ins.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResolutionStats {
    pub resolved: usize,
    pub unresolved: usize,
    pub builtin: usize,
}

impl ResolutionStats {
    pub fn from_cache(cache: &DashMap<(PathBuf, String), Option<PathBuf>>) -> Self {
        let mut stats = Self::default();
        for entry in cache.iter() {
            match (entry.value(), is_builtin(&entry.key().1)) {
                (_, true) => stats.builtin += 1,
                (Some(_), _) => stats.resolved += 1,
                (None, _) => stats.unresolved += 1,
            }
        }
        stats
    }

    /// Percentage of non-builtin imports that resolved to a file.
    pub fn resolved_percent(&self) -> f64 {
        let total = self.resolved + self.unresolved;
        if total == 0 { 100.0 } else { self.resolved as f64 * 100.0 / total as f64 }
    }
}

pub fn resolve(
    project: &Project,
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

    let root = project.root.as_path();
    let resolved = if request.starts_with("./")
        || request.starts_with("../")
        || request.starts_with('/')
        || request == "."
        || request == ".."
    {
        // Relative imports
        trace!("Resolving as relative import: '{}'", request);
        let base = from_file.parent().unwrap_or(root);
        let path = normalize(&base.join(request));
        let result =
            resolve_file(&path, Mode::Runtime).or_else(|| resolve_file(&path, Mode::Declaration));
        if result.is_some() {
            trace!("Resolved relative import '{}' to {:?}", request, result);
        } else {
            trace!("Failed to resolve relative import '{}'", request);
        }
        result
    } else if is_builtin(request) {
        trace!("Skipping built-in module '{}'", request);
        None
    } else {
        // As in TypeScript, a declaration file is a fallback: any runtime
        // file a later step of the lookup finds wins over it.
        resolve_bare(project, from_file, request, Mode::Runtime)
            .or_else(|| resolve_bare(project, from_file, request, Mode::Declaration))
    };

    cache.insert(key, resolved.clone());
    if resolved.is_some() {
        debug!("Successfully resolved '{}' from {}", request, from_file.display());
    }
    Ok(resolved)
}

/// Which files a lookup accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Files that run: never a declaration file.
    Runtime,
    /// Declaration files only, for when no runtime file exists.
    Declaration,
}

fn resolve_bare(project: &Project, from_file: &Path, request: &str, mode: Mode) -> Option<PathBuf> {
    let root = project.root.as_path();

    // 1. tsconfig `paths` and `baseUrl` from the file's nearest tsconfig.json
    if let Some(tsconfig) = project.tsconfig_for(from_file) {
        for candidate in tsconfig.alias_candidates(request) {
            if let Some(resolved) = resolve_file(&candidate, mode) {
                trace!("Resolved alias '{}' to {:?}", request, resolved);
                return Some(resolved);
            }
        }
        if let Some(base_url) = &tsconfig.base_url
            && let Some(resolved) = resolve_file(&normalize(&base_url.join(request)), mode)
        {
            trace!("Resolved '{}' via baseUrl to {:?}", request, resolved);
            return Some(resolved);
        }
    }

    // 2. Workspace packages, resolved to their sources. This comes before
    // node_modules so an installed workspace (a symlink in node_modules) still
    // resolves to the same source files as an uninstalled one.
    let (name, subpath) = split_package_request(request);
    if let Some(pkg_dir) = project.workspace_package(name)
        && let Some(resolved) = resolve_in_package(project, pkg_dir, subpath, mode)
    {
        trace!("Resolved workspace package '{}' to {:?}", request, resolved);
        return Some(resolved);
    }

    // 3. Installed packages, walking up from the importing file
    trace!("Resolving as node_modules package: '{}'", request);
    let mut dir = from_file.parent();
    while let Some(d) = dir {
        let pkg_dir = normalize(&d.join("node_modules").join(name));
        if pkg_dir.is_dir()
            && let Some(resolved) = resolve_in_package(project, &pkg_dir, subpath, mode)
        {
            trace!("Resolved node_modules package '{}' to {:?}", request, resolved);
            return Some(resolved);
        }
        if d == root {
            break;
        }
        dir = d.parent();
    }

    trace!("Failed to resolve node_modules package '{}'", request);
    None
}

/// Export conditions to try, in order. Type-only conditions are skipped.
const EXPORT_CONDITIONS: &[&str] =
    &["source", "import", "module", "require", "node", "react-native", "browser", "default"];

/// Resolve `subpath` ("" for the package root) inside a package directory.
fn resolve_in_package(
    project: &Project,
    pkg_dir: &Path,
    subpath: &str,
    mode: Mode,
) -> Option<PathBuf> {
    let pkg = project.package_json(pkg_dir);

    if let Some(exports) = pkg.as_ref().and_then(|p| p.get("exports"))
        && let Some(target) = match_exports(exports, subpath)
        && let Some(resolved) = resolve_package_target(project, pkg_dir, &target, mode)
    {
        return Some(resolved);
    }

    if subpath.is_empty() {
        for field in ["source", "module", "main"] {
            if let Some(target) = pkg.as_ref().and_then(|p| p.get(field)).and_then(|v| v.as_str())
                && let Some(resolved) = resolve_package_target(project, pkg_dir, target, mode)
            {
                return Some(resolved);
            }
        }
        return resolve_package_target(project, pkg_dir, "src/index", mode)
            .or_else(|| resolve_package_target(project, pkg_dir, "index", mode));
    }

    resolve_package_target(project, pkg_dir, subpath, mode)
}

/// Find the target for `subpath` in a package.json `exports` field.
fn match_exports(exports: &serde_json::Value, subpath: &str) -> Option<String> {
    let key = if subpath.is_empty() { ".".to_string() } else { format!("./{}", subpath) };

    let is_subpath_map = exports.as_object().is_some_and(|o| o.keys().any(|k| k.starts_with('.')));
    if !is_subpath_map {
        // Sugar: `"exports": "./index.js"` or a conditions object for "."
        return if subpath.is_empty() { pick_condition(exports, "") } else { None };
    }

    let map = exports.as_object()?;
    if let Some(v) = map.get(&key) {
        return pick_condition(v, "");
    }
    // Subpath patterns such as "./*" or "./utils/*.js"
    let mut best: Option<(&str, &serde_json::Value, String)> = None;
    for (k, v) in map {
        if let Some((prefix, suffix)) = k.split_once('*')
            && key.len() >= prefix.len() + suffix.len()
            && key.starts_with(prefix)
            && key.ends_with(suffix)
            && best.as_ref().is_none_or(|(p, _, _)| prefix.len() > p.len())
        {
            best = Some((prefix, v, key[prefix.len()..key.len() - suffix.len()].to_string()));
        }
    }
    let (_, v, star) = best?;
    pick_condition(v, &star)
}

fn pick_condition(v: &serde_json::Value, star: &str) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.replace('*', star)),
        serde_json::Value::Array(items) => items.iter().find_map(|i| pick_condition(i, star)),
        serde_json::Value::Object(o) => {
            EXPORT_CONDITIONS.iter().filter_map(|c| o.get(*c)).find_map(|i| pick_condition(i, star))
        }
        _ => None,
    }
}

/// Directories that usually hold build output mirroring `src/`.
const BUILD_DIRS: &[&str] = &["dist", "build", "lib", "esm", "cjs", "out"];

/// Resolve a path inside a package. If it points into build output that does not
/// exist (an unbuilt workspace package), fall back to the matching file under `src/`.
fn resolve_package_target(
    project: &Project,
    pkg_dir: &Path,
    target: &str,
    mode: Mode,
) -> Option<PathBuf> {
    let target = target.trim_start_matches("./");
    if let Some(resolved) = resolve_file(&normalize(&pkg_dir.join(target)), mode) {
        return Some(resolved);
    }
    if let Some(source) = source_for_out_dir(project, pkg_dir, target)
        && let Some(resolved) = resolve_file(&source, mode)
    {
        return Some(resolved);
    }

    let source = source_for_build_target(target)?;
    resolve_file(&normalize(&pkg_dir.join(source)), mode)
}

/// The source for a target in the `outDir` of the package's tsconfig, as
/// knip maps it: under `rootDir`, or else `src` if the package has one.
/// `development/a.js` is `src/a` with `"outDir": "development"`.
fn source_for_out_dir(project: &Project, pkg_dir: &Path, target: &str) -> Option<PathBuf> {
    let cfg = project.tsconfig_for(&pkg_dir.join("package.json"))?;
    let out_dir = cfg.out_dir.as_deref()?;
    let rest = normalize(&pkg_dir.join(target));
    let rest = rest.strip_prefix(out_dir).ok()?.to_str()?;
    let src_dir = match &cfg.root_dir {
        Some(root_dir) => root_dir.clone(),
        // tsc without rootDir mirrors the package directory, src/ included.
        None if out_dir.join("src").is_dir() => pkg_dir.to_path_buf(),
        None if pkg_dir.join("src").is_dir() => pkg_dir.join("src"),
        None => pkg_dir.to_path_buf(),
    };
    (src_dir != out_dir).then(|| src_dir.join(strip_compiled_extension(rest)))
}

/// `src/a/b` for a target in build output such as `dist/esm/a/b.js`.
fn source_for_build_target(target: &str) -> Option<String> {
    let (first, rest) = target.split_once('/')?;
    if !BUILD_DIRS.contains(&first) {
        return None;
    }
    // Drop build-only subdirectories like dist/esm/... and the compiled extension
    let rest =
        BUILD_DIRS.iter().find_map(|d| rest.strip_prefix(&format!("{}/", d))).unwrap_or(rest);
    Some(format!("src/{}", strip_compiled_extension(rest)))
}

fn strip_compiled_extension(path: &str) -> &str {
    [".d.ts", ".d.mts", ".d.cts", ".js", ".mjs", ".cjs", ".jsx"]
        .iter()
        .find_map(|ext| path.strip_suffix(ext))
        .unwrap_or(path)
}

/// The files a package.json target such as `./dist/index.js` points to, as
/// [`resolve`] finds them, preferring runtime files. A target with `*`, as
/// in `"./*": "./src/*.ts"`, matches every source file it can stand for,
/// `*` matching across directories as in `exports` patterns.
pub(crate) fn package_target_files(
    project: &Project,
    pkg_dir: &Path,
    target: &str,
) -> Vec<PathBuf> {
    let target = target.trim_start_matches("./");
    if !target.contains('*') {
        return resolve_package_target(project, pkg_dir, target, Mode::Runtime)
            .or_else(|| resolve_package_target(project, pkg_dir, target, Mode::Declaration))
            .into_iter()
            .collect();
    }
    let out_dir_source = source_for_out_dir(project, pkg_dir, target)
        .and_then(|p| Some(p.strip_prefix(pkg_dir).ok()?.to_str()?.replace('\\', "/")));
    let candidates = std::iter::once(strip_compiled_extension(target).to_string())
        .chain(out_dir_source)
        .chain(source_for_build_target(target));
    for stem in candidates {
        let files = files_matching_stem(pkg_dir, &stem);
        if !files.is_empty() {
            return files;
        }
    }
    Vec::new()
}

/// Source files under `dir` whose path without its extension matches `stem`,
/// a path with `*` wildcards. Already-present extensions such as `.ts` in
/// `src/*.ts` match too.
fn files_matching_stem(dir: &Path, stem: &str) -> Vec<PathBuf> {
    let exts = JS_TS_EXTENSIONS.join(",");
    let pattern = if JS_TS_EXTENSIONS.iter().any(|e| stem.ends_with(&format!(".{e}"))) {
        stem.to_string()
    } else {
        format!("{stem}{{.{{{exts}}},/index.{{{exts}}}}}")
    };
    let Ok(glob) = globset::GlobBuilder::new(&pattern).literal_separator(false).build() else {
        trace!("Invalid package target pattern '{}'", pattern);
        return Vec::new();
    };
    let matcher = glob.compile_matcher();
    let walker = ignore::WalkBuilder::new(dir)
        .require_git(false)
        .filter_entry(|e| e.file_name() != "node_modules" && e.file_name() != ".git")
        .build();
    let mut files: Vec<PathBuf> = walker
        .filter_map(std::result::Result::ok)
        .map(ignore::DirEntry::into_path)
        .filter(|p| p.is_file() && p.strip_prefix(dir).is_ok_and(|rel| matcher.is_match(rel)))
        .filter_map(canonical)
        .collect();
    files.sort();
    files
}

/// Resolves `p` to a runtime file, or with [`Mode::Declaration`] to a
/// declaration file, as TypeScript does: `p` itself, `p.d.ts` (or `.d.mts`,
/// `.d.cts`), `a.d.ts` for `a.js` (`a.d.mts` for `a.mjs`, `a.d.cts` for
/// `a.cjs`), or `p/index.d.ts`.
fn resolve_file(p: &Path, mode: Mode) -> Option<PathBuf> {
    let found = resolve_runtime_file(p);
    match mode {
        Mode::Runtime => found.filter(|f| !is_declaration(f)),
        Mode::Declaration => found.filter(|f| is_declaration(f)).or_else(|| resolve_declaration(p)),
    }
}

fn resolve_declaration(p: &Path) -> Option<PathBuf> {
    let base = p.display().to_string();
    // `./a.js` is described by `./a.d.ts`.
    let stem = [".js", ".jsx", ".mjs", ".cjs"]
        .iter()
        .find_map(|ext| base.strip_suffix(ext).map(|stem| (stem, *ext)));
    let candidates: Vec<PathBuf> = match stem {
        Some((stem, ".mjs")) => vec![PathBuf::from(format!("{stem}.d.mts"))],
        Some((stem, ".cjs")) => vec![PathBuf::from(format!("{stem}.d.cts"))],
        Some((stem, _)) => vec![PathBuf::from(format!("{stem}.d.ts"))],
        None => DECLARATION_SUFFIXES
            .iter()
            .map(|s| PathBuf::from(format!("{base}{s}")))
            .chain(DECLARATION_SUFFIXES.iter().map(|s| p.join(format!("index{s}"))))
            .collect(),
    };
    candidates.into_iter().find(|c| c.is_file()).and_then(canonical)
}

fn resolve_runtime_file(p: &Path) -> Option<PathBuf> {
    if let Some(resolved) = resolve_as_file(p) {
        return Some(resolved);
    }

    // Directory: a package.json `main` or an index file
    if p.is_dir() {
        if let Some(main) = fs::read_to_string(p.join("package.json"))
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.get("main").and_then(|m| m.as_str()).map(str::to_string))
        {
            let main_path = normalize(&p.join(&main));
            if let Some(resolved) =
                resolve_as_file(&main_path).or_else(|| resolve_index(&main_path))
            {
                return Some(resolved);
            }
        }
        return resolve_index(p);
    }

    None
}

fn canonical(p: PathBuf) -> Option<PathBuf> {
    Some(p.canonicalize().unwrap_or(p))
}

fn resolve_as_file(p: &Path) -> Option<PathBuf> {
    // Try exact path first (but only if it's a file, not a directory)
    if p.is_file() {
        return canonical(p.to_path_buf());
    }

    // Try adding extensions
    for ext in RESOLVE_EXTENSIONS {
        let candidate = PathBuf::from(format!("{}.{}", p.display(), ext));
        if candidate.is_file() {
            return canonical(candidate);
        }
    }

    // TypeScript ESM style: `./foo.js` refers to `./foo.ts`
    if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
        let swaps: &[&str] = match ext {
            "js" => &["ts", "tsx"],
            "jsx" => &["tsx"],
            "mjs" => &["mts"],
            "cjs" => &["cts"],
            _ => &[],
        };
        for swap in swaps {
            let candidate = p.with_extension(swap);
            if candidate.is_file() {
                return canonical(candidate);
            }
        }
    }

    None
}

fn resolve_index(dir: &Path) -> Option<PathBuf> {
    INDEX_FILES.iter().map(|f| dir.join(f)).find(|c| c.is_file()).and_then(canonical)
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
    fn test_resolve_relative_same_dir() {
        let temp_dir = TempDir::new().unwrap();
        let cache = DashMap::new();
        let root = temp_dir.path();
        let from_file = create_test_file(root, "src/file.js", "// test");
        let target_file = create_test_file(root, "src/utils.js", "// utils");

        let resolved = resolve(&Project::new(root), &from_file, "./utils", &cache).unwrap();
        assert!(resolved.is_some());
        // Normalize paths for comparison (canonicalize can add /private prefix on macOS)
        assert_eq!(resolved.unwrap().canonicalize().unwrap(), target_file.canonicalize().unwrap());
    }

    #[test]
    fn declaration_files_resolve_after_runtime_files() {
        let temp_dir = TempDir::new().unwrap();
        let cache = DashMap::new();
        let root = temp_dir.path();
        let from = create_test_file(root, "src/main.ts", "");
        create_test_file(root, "src/types.d.ts", "");
        create_test_file(root, "src/lib.js", "");
        create_test_file(root, "src/lib.d.ts", "");
        create_test_file(root, "src/esm.d.mts", "");
        create_test_file(root, "src/decl/index.d.ts", "");
        create_test_file(root, "src/both.d.ts", "");
        create_test_file(root, "src/both/index.ts", "");
        let project = Project::new(root);
        let got = |request: &str| {
            resolve(&project, &from, request, &cache)
                .unwrap()
                .map(|p| p.strip_prefix(root.canonicalize().unwrap()).unwrap().to_path_buf())
        };
        let want = |p: &str| Some(PathBuf::from(p));
        assert_eq!(got("./types"), want("src/types.d.ts"));
        assert_eq!(got("./types.js"), want("src/types.d.ts"));
        assert_eq!(got("./esm.mjs"), want("src/esm.d.mts"));
        assert_eq!(got("./decl"), want("src/decl/index.d.ts"));
        // Runtime files come first, a directory's index included.
        assert_eq!(got("./lib"), want("src/lib.js"));
        assert_eq!(got("./both"), want("src/both/index.ts"));
        assert_eq!(got("./missing"), None);
    }

    #[test]
    fn test_resolve_relative_parent_dir() {
        let temp_dir = TempDir::new().unwrap();
        let cache = DashMap::new();
        let root = temp_dir.path();
        let from_file = create_test_file(root, "src/components/Button.js", "// test");
        let target_file = create_test_file(root, "src/utils.js", "// utils");

        let resolved = resolve(&Project::new(root), &from_file, "../utils", &cache).unwrap();
        assert!(resolved.is_some());
        // Normalize paths for comparison (canonicalize can add /private prefix on macOS)
        assert_eq!(resolved.unwrap().canonicalize().unwrap(), target_file.canonicalize().unwrap());
    }

    #[test]
    fn test_resolve_with_extension() {
        let temp_dir = TempDir::new().unwrap();
        let cache = DashMap::new();
        let root = temp_dir.path();
        let from_file = create_test_file(root, "src/file.js", "// test");
        let target_file = create_test_file(root, "src/utils.ts", "// utils");

        // Request without extension should resolve to .ts file
        let resolved = resolve(&Project::new(root), &from_file, "./utils", &cache).unwrap();
        assert!(resolved.is_some());
        // Normalize paths for comparison (canonicalize can add /private prefix on macOS)
        assert_eq!(resolved.unwrap().canonicalize().unwrap(), target_file.canonicalize().unwrap());
    }

    #[test]
    fn test_resolve_index_file() {
        let temp_dir = TempDir::new().unwrap();
        let cache = DashMap::new();
        let root = temp_dir.path();
        let from_file = create_test_file(root, "src/file.js", "// test");
        let target_file = create_test_file(root, "src/utils/index.js", "// utils");

        // Request directory should resolve to index.js
        let resolved = resolve(&Project::new(root), &from_file, "./utils", &cache).unwrap();
        assert!(resolved.is_some());
        // Normalize paths for comparison (canonicalize can add /private prefix on macOS)
        assert_eq!(resolved.unwrap().canonicalize().unwrap(), target_file.canonicalize().unwrap());
    }

    #[test]
    fn test_resolve_tsconfig_path_alias() {
        let temp_dir = TempDir::new().unwrap();
        let cache = DashMap::new();
        let root_buf = temp_dir.path().canonicalize().unwrap();
        let root = root_buf.as_path();
        create_test_file(
            root,
            "tsconfig.json",
            r#"{"compilerOptions":{"paths":{"@components/*":["./src/components/*"]}}}"#,
        );
        let from_file = create_test_file(root, "src/file.js", "// test");
        let target_file = create_test_file(root, "src/components/Button.js", "// button");

        let project = Project::load(root);
        let resolved = resolve(&project, &from_file, "@components/Button", &cache).unwrap();
        assert!(resolved.is_some());
        // Normalize paths for comparison
        assert_eq!(resolved.unwrap().canonicalize().unwrap(), target_file.canonicalize().unwrap());
    }

    #[test]
    fn test_resolve_tsconfig_alias_scoped_to_nearest_tsconfig() {
        let temp_dir = TempDir::new().unwrap();
        let cache = DashMap::new();
        let root_buf = temp_dir.path().canonicalize().unwrap();
        let root = root_buf.as_path();
        create_test_file(
            root,
            "a/tsconfig.json",
            r#"{"compilerOptions":{"paths":{"~/*":["./src/*"]}}}"#,
        );
        create_test_file(root, "a/src/x.ts", "// x");
        let from_a = create_test_file(root, "a/src/index.ts", "// a");
        let from_b = create_test_file(root, "b/src/index.ts", "// b");

        let project = Project::load(root);
        assert!(resolve(&project, &from_a, "~/x", &cache).unwrap().is_some());
        // Package b has no such alias
        assert!(resolve(&project, &from_b, "~/x", &cache).unwrap().is_none());
    }

    #[test]
    fn test_resolve_base_url() {
        let temp_dir = TempDir::new().unwrap();
        let cache = DashMap::new();
        let root_buf = temp_dir.path().canonicalize().unwrap();
        let root = root_buf.as_path();
        create_test_file(root, "tsconfig.json", r#"{"compilerOptions":{"baseUrl":"src"}}"#);
        let from_file = create_test_file(root, "src/pages/index.ts", "// page");
        let target = create_test_file(root, "src/components/Button.tsx", "// button");

        let project = Project::load(root);
        let resolved = resolve(&project, &from_file, "components/Button", &cache).unwrap();
        assert_eq!(resolved.unwrap(), target.canonicalize().unwrap());
    }

    #[test]
    fn test_resolve_ts_esm_js_extension() {
        let temp_dir = TempDir::new().unwrap();
        let cache = DashMap::new();
        let root = temp_dir.path();
        let from_file = create_test_file(root, "src/file.ts", "// test");
        let target = create_test_file(root, "src/utils.ts", "// utils");

        let resolved = resolve(&Project::new(root), &from_file, "./utils.js", &cache).unwrap();
        assert_eq!(resolved.unwrap(), target.canonicalize().unwrap());
    }

    #[test]
    fn test_resolve_prefers_file_over_directory() {
        let temp_dir = TempDir::new().unwrap();
        let cache = DashMap::new();
        let root = temp_dir.path();
        let from_file = create_test_file(root, "src/file.ts", "// test");
        let target = create_test_file(root, "src/utils.ts", "// utils file");
        create_test_file(root, "src/utils/index.ts", "// utils dir");

        let resolved = resolve(&Project::new(root), &from_file, "./utils", &cache).unwrap();
        assert_eq!(resolved.unwrap(), target.canonicalize().unwrap());
    }

    #[test]
    fn test_builtins_are_not_resolved() {
        let temp_dir = TempDir::new().unwrap();
        let cache = DashMap::new();
        let root = temp_dir.path();
        let from_file = create_test_file(root, "src/file.ts", "// test");
        // Even a same-named package in node_modules must not shadow a builtin
        create_test_file(root, "node_modules/fs/index.js", "// fake");

        let project = Project::new(root);
        assert!(is_builtin("fs/promises"));
        assert!(is_builtin("node:path"));
        assert!(!is_builtin("fs-extra"));
        assert!(resolve(&project, &from_file, "fs", &cache).unwrap().is_none());
        assert!(resolve(&project, &from_file, "node:fs", &cache).unwrap().is_none());
    }

    #[test]
    fn test_resolve_node_modules_subpath_file() {
        let temp_dir = TempDir::new().unwrap();
        let cache = DashMap::new();
        let root = temp_dir.path();
        let from_file = create_test_file(root, "src/file.js", "// test");
        create_test_file(root, "node_modules/lodash/package.json", r#"{"main":"lodash.js"}"#);
        let target = create_test_file(root, "node_modules/lodash/debounce.js", "// debounce");

        let resolved = resolve(&Project::new(root), &from_file, "lodash/debounce", &cache).unwrap();
        assert_eq!(resolved.unwrap(), target.canonicalize().unwrap());
    }

    #[test]
    fn test_resolve_exports_conditions_and_patterns() {
        let temp_dir = TempDir::new().unwrap();
        let cache = DashMap::new();
        let root = temp_dir.path();
        let from_file = create_test_file(root, "src/file.js", "// test");
        create_test_file(
            root,
            "node_modules/@scope/pkg/package.json",
            r#"{"exports":{".":{"types":"./index.d.ts","import":"./esm/index.mjs"},"./features/*":"./esm/features/*.mjs"}}"#,
        );
        let main = create_test_file(root, "node_modules/@scope/pkg/esm/index.mjs", "// main");
        let feat = create_test_file(root, "node_modules/@scope/pkg/esm/features/a.mjs", "// a");

        let project = Project::new(root);
        let resolved = resolve(&project, &from_file, "@scope/pkg", &cache).unwrap();
        assert_eq!(resolved.unwrap(), main.canonicalize().unwrap());
        let resolved = resolve(&project, &from_file, "@scope/pkg/features/a", &cache).unwrap();
        assert_eq!(resolved.unwrap(), feat.canonicalize().unwrap());
    }

    #[test]
    fn declaration_files_lose_to_runtime_files_found_later() {
        let temp_dir = TempDir::new().unwrap();
        let cache = DashMap::new();
        let root_buf = temp_dir.path().canonicalize().unwrap();
        let root = root_buf.as_path();
        // A local `.d.ts` shim for a package, mapped by tsconfig `paths`.
        create_test_file(
            root,
            "tsconfig.json",
            r#"{"compilerOptions":{"paths":{"*":["types/*"]}}}"#,
        );
        create_test_file(root, "types/heavy/index.d.ts", "");
        create_test_file(root, "types/shim-only/index.d.ts", "");
        let heavy = create_test_file(root, "node_modules/heavy/index.js", "");
        // A workspace package whose types are built but not its JavaScript.
        create_test_file(root, "pnpm-workspace.yaml", "packages:\n  - packages/*\n");
        create_test_file(
            root,
            "packages/pkg/package.json",
            r#"{"name":"pkg","main":"./dist/index.js"}"#,
        );
        create_test_file(root, "packages/pkg/dist/index.d.ts", "");
        let pkg_src = create_test_file(root, "packages/pkg/src/index.ts", "");
        let from = create_test_file(root, "src/main.ts", "");

        let project = Project::load(root);
        let got = |request: &str| resolve(&project, &from, request, &cache).unwrap();
        assert_eq!(got("heavy"), Some(heavy));
        assert_eq!(got("pkg"), Some(pkg_src));
        // With no runtime file anywhere, the declaration file is used.
        assert_eq!(got("shim-only"), Some(root.join("types/shim-only/index.d.ts")));
    }

    #[test]
    fn unbuilt_targets_in_tsconfig_out_dir_map_to_root_dir() {
        let temp_dir = TempDir::new().unwrap();
        let cache = DashMap::new();
        let root_buf = temp_dir.path().canonicalize().unwrap();
        let root = root_buf.as_path();
        create_test_file(root, "pnpm-workspace.yaml", "packages:\n  - packages/*\n");
        // As in lit: `development` is not a usual build directory name.
        create_test_file(
            root,
            "packages/lit/package.json",
            r#"{"name":"lit","exports":{".":"./development/index.js","./directive.js":"./development/directive.js"}}"#,
        );
        create_test_file(
            root,
            "packages/lit/tsconfig.json",
            r#"{"compilerOptions":{"outDir":"./development","rootDir":"./source"}}"#,
        );
        let index = create_test_file(root, "packages/lit/source/index.ts", "");
        let directive = create_test_file(root, "packages/lit/source/directive.ts", "");
        let from_file = create_test_file(root, "packages/app/src/index.ts", "");

        let project = Project::load(root);
        let got = |r: &str| resolve(&project, &from_file, r, &cache).unwrap();
        assert_eq!(got("lit"), Some(index));
        assert_eq!(got("lit/directive.js"), Some(directive.clone()));
        assert_eq!(
            package_target_files(&project, &root.join("packages/lit"), "./development/*.js"),
            [directive, root.join("packages/lit/source/index.ts")]
        );
    }

    #[test]
    fn test_resolve_unbuilt_workspace_package_to_source() {
        let temp_dir = TempDir::new().unwrap();
        let cache = DashMap::new();
        let root_buf = temp_dir.path().canonicalize().unwrap();
        let root = root_buf.as_path();
        create_test_file(root, "pnpm-workspace.yaml", "packages:\n  - packages/*\n");
        create_test_file(
            root,
            "packages/ui/package.json",
            r#"{"name":"@acme/ui","main":"build/index.js"}"#,
        );
        let ui_index = create_test_file(root, "packages/ui/src/index.ts", "// ui");
        let ui_button = create_test_file(root, "packages/ui/src/Button.tsx", "// button");
        create_test_file(root, "packages/app/package.json", r#"{"name":"app"}"#);
        let from_file = create_test_file(root, "packages/app/src/index.ts", "// app");

        let project = Project::load(root);
        let resolved = resolve(&project, &from_file, "@acme/ui", &cache).unwrap();
        assert_eq!(resolved.unwrap(), ui_index.canonicalize().unwrap());
        // Deep import into build output maps back to the source file
        let resolved = resolve(&project, &from_file, "@acme/ui/build/Button.js", &cache).unwrap();
        assert_eq!(resolved.unwrap(), ui_button.canonicalize().unwrap());
    }

    #[test]
    fn test_resolve_nonexistent_file() {
        let temp_dir = TempDir::new().unwrap();
        let cache = DashMap::new();
        let root = temp_dir.path();
        let from_file = create_test_file(root, "src/file.js", "// test");

        let resolved = resolve(&Project::new(root), &from_file, "./nonexistent", &cache).unwrap();
        assert!(resolved.is_none());
    }

    #[test]
    fn test_resolve_cache_behavior() {
        let temp_dir = TempDir::new().unwrap();
        let cache = DashMap::new();
        let root = temp_dir.path();
        let from_file = create_test_file(root, "src/file.js", "// test");
        let _target_file = create_test_file(root, "src/utils.js", "// utils");

        // First call
        let resolved1 = resolve(&Project::new(root), &from_file, "./utils", &cache).unwrap();
        assert!(resolved1.is_some());

        // Second call should use cache
        let resolved2 = resolve(&Project::new(root), &from_file, "./utils", &cache).unwrap();
        assert!(resolved2.is_some());
        assert_eq!(resolved1.unwrap(), resolved2.unwrap());

        // Cache should have entry
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn test_resolve_node_modules_with_main() {
        let temp_dir = TempDir::new().unwrap();
        let cache = DashMap::new();
        let root = temp_dir.path();
        let from_file = create_test_file(root, "src/file.js", "// test");

        // Create node_modules structure
        let pkg_dir = root.join("node_modules").join("test-pkg");
        fs::create_dir_all(&pkg_dir).unwrap();
        let pkg_json = pkg_dir.join("package.json");
        fs::write(&pkg_json, r#"{"main": "lib/index.js"}"#).unwrap();
        let main_file = create_test_file(&pkg_dir, "lib/index.js", "// main");

        let resolved = resolve(&Project::new(root), &from_file, "test-pkg", &cache).unwrap();
        assert!(resolved.is_some());
        // Normalize paths for comparison
        assert_eq!(resolved.unwrap().canonicalize().unwrap(), main_file.canonicalize().unwrap());
    }

    #[test]
    fn test_resolve_node_modules_with_exports() {
        let temp_dir = TempDir::new().unwrap();
        let cache = DashMap::new();
        let root = temp_dir.path();
        let from_file = create_test_file(root, "src/file.js", "// test");

        // Create node_modules structure with exports field
        let pkg_dir = root.join("node_modules").join("test-pkg");
        fs::create_dir_all(&pkg_dir).unwrap();
        let pkg_json = pkg_dir.join("package.json");
        fs::write(&pkg_json, r#"{"exports": "./dist/index.js"}"#).unwrap();
        let main_file = create_test_file(&pkg_dir, "dist/index.js", "// main");

        let resolved = resolve(&Project::new(root), &from_file, "test-pkg", &cache).unwrap();
        assert!(resolved.is_some());
        // Normalize paths for comparison
        assert_eq!(resolved.unwrap().canonicalize().unwrap(), main_file.canonicalize().unwrap());
    }

    #[test]
    fn test_resolve_node_modules_with_exports_object() {
        let temp_dir = TempDir::new().unwrap();
        let cache = DashMap::new();
        let root = temp_dir.path();
        let from_file = create_test_file(root, "src/file.js", "// test");

        // Create node_modules structure with exports object
        let pkg_dir = root.join("node_modules").join("test-pkg");
        fs::create_dir_all(&pkg_dir).unwrap();
        let pkg_json = pkg_dir.join("package.json");
        fs::write(&pkg_json, r#"{"exports": {".": "./dist/index.js"}}"#).unwrap();
        let main_file = create_test_file(&pkg_dir, "dist/index.js", "// main");

        let resolved = resolve(&Project::new(root), &from_file, "test-pkg", &cache).unwrap();
        assert!(resolved.is_some());
        // Normalize paths for comparison
        assert_eq!(resolved.unwrap().canonicalize().unwrap(), main_file.canonicalize().unwrap());
    }

    #[test]
    fn test_resolve_node_modules_fallback_to_index() {
        let temp_dir = TempDir::new().unwrap();
        let cache = DashMap::new();
        let root = temp_dir.path();
        let from_file = create_test_file(root, "src/file.js", "// test");

        // Create node_modules structure without package.json main/exports
        let pkg_dir = root.join("node_modules").join("test-pkg");
        fs::create_dir_all(&pkg_dir).unwrap();
        let index_file = create_test_file(&pkg_dir, "index.js", "// index");

        let resolved = resolve(&Project::new(root), &from_file, "test-pkg", &cache).unwrap();
        assert!(resolved.is_some());
        // Normalize paths for comparison
        assert_eq!(resolved.unwrap().canonicalize().unwrap(), index_file.canonicalize().unwrap());
    }

    #[test]
    fn test_resolve_node_modules_walks_up() {
        let temp_dir = TempDir::new().unwrap();
        let cache = DashMap::new();
        let root = temp_dir.path();
        // Create node_modules at root, not in subdirectory
        let pkg_dir = root.join("node_modules").join("test-pkg");
        fs::create_dir_all(&pkg_dir).unwrap();
        let index_file = create_test_file(&pkg_dir, "index.js", "// index");

        // File in subdirectory should still find root node_modules
        let from_file = create_test_file(root, "src/nested/deep/file.js", "// test");

        let resolved = resolve(&Project::new(root), &from_file, "test-pkg", &cache).unwrap();
        assert!(resolved.is_some());
        // Normalize paths for comparison
        assert_eq!(resolved.unwrap().canonicalize().unwrap(), index_file.canonicalize().unwrap());
    }
}
