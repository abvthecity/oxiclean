//! Project-wide context used for module resolution.
//!
//! A [`Project`] is loaded once per run and records:
//! - every `tsconfig.json` in the tree, so each file resolves aliases against
//!   its *nearest* tsconfig (following `extends`), as TypeScript does
//! - every workspace package (pnpm-workspace.yaml or package.json `workspaces`),
//!   so imports between packages of a monorepo resolve to their sources even
//!   when `node_modules` has not been installed

use dashmap::DashMap;
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use ignore::WalkBuilder;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};
use tracing::{debug, trace};

use crate::jsonc::parse_jsonc;

/// A single `compilerOptions.paths` entry, e.g. `"@/*": ["./src/*"]`.
#[derive(Debug, Clone)]
pub struct PathAlias {
    prefix: String,
    /// `Some(suffix)` for wildcard patterns, `None` for exact patterns
    suffix: Option<String>,
    /// Absolute target paths, possibly containing a single `*`
    targets: Vec<String>,
}

/// The resolution-relevant parts of a tsconfig, after applying `extends`.
#[derive(Debug, Clone, Default)]
pub struct TsConfig {
    pub base_url: Option<PathBuf>,
    pub paths: Vec<PathAlias>,
    /// `compilerOptions.outDir`, where the compiled files go.
    pub out_dir: Option<PathBuf>,
    /// `compilerOptions.rootDir`, the sources `out_dir` mirrors.
    pub root_dir: Option<PathBuf>,
}

impl TsConfig {
    /// Return candidate paths for `request` from the best matching alias, following
    /// TypeScript's rules: an exact match wins, otherwise the longest wildcard prefix.
    pub fn alias_candidates(&self, request: &str) -> Vec<PathBuf> {
        let mut best: Option<(&PathAlias, &str)> = None;
        for alias in &self.paths {
            match &alias.suffix {
                None if alias.prefix == request => {
                    return alias.targets.iter().map(PathBuf::from).collect();
                }
                Some(suffix)
                    if request.len() >= alias.prefix.len() + suffix.len()
                        && request.starts_with(&alias.prefix)
                        && request.ends_with(suffix.as_str()) =>
                {
                    let star = &request[alias.prefix.len()..request.len() - suffix.len()];
                    if best.is_none_or(|(b, _)| alias.prefix.len() > b.prefix.len()) {
                        best = Some((alias, star));
                    }
                }
                _ => {}
            }
        }
        match best {
            Some((alias, star)) => alias
                .targets
                .iter()
                .map(|t| normalize(Path::new(&t.replacen('*', star, 1))))
                .collect(),
            None => Vec::new(),
        }
    }
}

#[derive(Debug, Default)]
pub struct Project {
    pub root: PathBuf,
    /// Directory containing a tsconfig.json -> its effective config
    tsconfigs: HashMap<PathBuf, Arc<TsConfig>>,
    /// Workspace package name -> package directory
    workspace_packages: HashMap<String, PathBuf>,
    package_json_cache: DashMap<PathBuf, Option<Arc<serde_json::Value>>>,
}

impl Project {
    /// A project with no tsconfig or workspace information.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into(), ..Default::default() }
    }

    /// Scan `root` for tsconfig.json files and workspace packages.
    pub fn load(root: impl Into<PathBuf>) -> Self {
        let root: PathBuf = root.into();
        let mut project = Self::new(root.canonicalize().unwrap_or(root));
        let root = project.root.clone();

        let mut tsconfig_files = Vec::new();
        let mut package_dirs = Vec::new();
        let walker = WalkBuilder::new(&root)
            .hidden(false)
            .git_ignore(true)
            .filter_entry(|e| e.file_name() != "node_modules" && e.file_name() != ".git")
            .build();
        for entry in walker.filter_map(|e| e.ok()) {
            match entry.file_name().to_str() {
                Some("tsconfig.json") => tsconfig_files.push(entry.into_path()),
                Some("package.json" | "deno.json" | "deno.jsonc") => {
                    if let Some(dir) = entry.path().parent() {
                        package_dirs.push(dir.to_path_buf());
                    }
                }
                _ => {}
            }
        }
        tsconfig_files.sort();
        package_dirs.sort();
        package_dirs.dedup();

        project.workspace_packages = discover_workspace_packages(&root, &package_dirs);
        debug!("Found {} workspace packages", project.workspace_packages.len());

        let mut loaded: HashMap<PathBuf, Option<TsConfig>> = HashMap::new();
        for file in &tsconfig_files {
            let cfg = project.load_tsconfig(file, &mut loaded, 0).unwrap_or_default();
            if let Some(dir) = file.parent() {
                project.tsconfigs.insert(dir.to_path_buf(), Arc::new(cfg));
            }
        }
        debug!("Loaded {} tsconfig.json files", project.tsconfigs.len());

        project
    }

    /// The effective tsconfig for `file`: the nearest tsconfig.json in an ancestor directory.
    pub fn tsconfig_for(&self, file: &Path) -> Option<&TsConfig> {
        let mut dir = file.parent();
        while let Some(d) = dir {
            if let Some(cfg) = self.tsconfigs.get(d) {
                return Some(cfg);
            }
            if d == self.root {
                break;
            }
            dir = d.parent();
        }
        None
    }

    /// The targets of tsconfig `paths` aliases named after a workspace
    /// package, such as `"@acme/ui": ["./packages/ui/lib/index.mjs"]` or
    /// `"@acme/ui/*": ["./packages/ui/lib/*.mjs"]`: absolute paths, possibly
    /// with one `*`. Code importing the package loads them, so they are the
    /// package's interface even when its package.json points elsewhere.
    pub(crate) fn package_alias_targets(&self) -> Vec<String> {
        let mut targets: Vec<String> = self
            .tsconfigs
            .values()
            .flat_map(|cfg| &cfg.paths)
            .filter(|alias| {
                let name = match &alias.suffix {
                    None => alias.prefix.as_str(),
                    Some(_) => alias.prefix.trim_end_matches('/'),
                };
                self.workspace_packages.contains_key(name)
            })
            .flat_map(|alias| alias.targets.iter().cloned())
            .collect();
        targets.sort();
        targets.dedup();
        targets
    }

    /// Directory of the workspace package called `name`, if any.
    pub fn workspace_package(&self, name: &str) -> Option<&Path> {
        self.workspace_packages.get(name).map(PathBuf::as_path)
    }

    pub fn workspace_package_count(&self) -> usize {
        self.workspace_packages.len()
    }

    /// Workspace packages as (name, directory) pairs, sorted by name.
    pub fn workspace_packages(&self) -> Vec<(&str, &Path)> {
        let mut v: Vec<(&str, &Path)> =
            self.workspace_packages.iter().map(|(n, d)| (n.as_str(), d.as_path())).collect();
        v.sort_unstable();
        v
    }

    /// Name of the workspace package whose directory is the nearest ancestor
    /// of `file`, if any.
    pub fn package_for(&self, file: &Path) -> Option<&str> {
        self.workspace_packages
            .iter()
            .filter(|(_, dir)| file.starts_with(dir))
            .max_by_key(|(_, dir)| dir.components().count())
            .map(|(name, _)| name.as_str())
    }

    /// Parsed package.json in `dir`, cached across calls.
    pub fn package_json(&self, dir: &Path) -> Option<Arc<serde_json::Value>> {
        if let Some(v) = self.package_json_cache.get(dir) {
            return v.clone();
        }
        let v = fs::read_to_string(dir.join("package.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .map(Arc::new);
        self.package_json_cache.insert(dir.to_path_buf(), v.clone());
        v
    }

    fn load_tsconfig(
        &self,
        file: &Path,
        loaded: &mut HashMap<PathBuf, Option<TsConfig>>,
        depth: usize,
    ) -> Option<TsConfig> {
        if let Some(cfg) = loaded.get(file) {
            return cfg.clone();
        }
        // Guard against `extends` cycles
        if depth > 16 {
            return None;
        }
        loaded.insert(file.to_path_buf(), None);

        let json = fs::read_to_string(file).ok().and_then(|s| parse_jsonc(&s));
        let Some(json) = json else {
            trace!("Could not parse tsconfig at {}", file.display());
            return None;
        };
        let dir = file.parent().unwrap_or(&self.root);

        // Start from the extended config(s); later entries override earlier ones
        let mut cfg = TsConfig::default();
        let extends: Vec<&str> = match json.get("extends") {
            Some(serde_json::Value::String(s)) => vec![s.as_str()],
            Some(serde_json::Value::Array(a)) => a.iter().filter_map(|v| v.as_str()).collect(),
            _ => vec![],
        };
        for ext in extends {
            if let Some(path) = self.resolve_extends(dir, ext)
                && let Some(parent) = self.load_tsconfig(&path, loaded, depth + 1)
            {
                if parent.base_url.is_some() {
                    cfg.base_url = parent.base_url;
                }
                if !parent.paths.is_empty() {
                    cfg.paths = parent.paths;
                }
                if parent.out_dir.is_some() {
                    cfg.out_dir = parent.out_dir;
                }
                if parent.root_dir.is_some() {
                    cfg.root_dir = parent.root_dir;
                }
            }
        }

        let opts = json.get("compilerOptions");
        if let Some(base_url) = opts.and_then(|o| o.get("baseUrl")).and_then(|b| b.as_str()) {
            cfg.base_url = Some(normalize(&dir.join(base_url)));
        }
        let dir_option = |key: &str| {
            opts.and_then(|o| o.get(key)).and_then(|v| v.as_str()).map(|v| normalize(&dir.join(v)))
        };
        if let Some(out_dir) = dir_option("outDir") {
            cfg.out_dir = Some(out_dir);
        }
        if let Some(root_dir) = dir_option("rootDir") {
            cfg.root_dir = Some(root_dir);
        }
        if let Some(paths) = opts.and_then(|o| o.get("paths")).and_then(|p| p.as_object()) {
            // Paths are relative to baseUrl if set, otherwise to the declaring tsconfig
            let base = cfg.base_url.clone().unwrap_or_else(|| dir.to_path_buf());
            cfg.paths = paths
                .iter()
                .filter_map(|(pattern, targets)| {
                    let targets: Vec<String> = targets
                        .as_array()?
                        .iter()
                        .filter_map(|t| t.as_str())
                        .map(|t| normalize(&base.join(t)).to_string_lossy().into_owned())
                        .collect();
                    let (prefix, suffix) = match pattern.split_once('*') {
                        Some((p, s)) => (p.to_string(), Some(s.to_string())),
                        None => (pattern.clone(), None),
                    };
                    trace!("tsconfig alias '{}' -> {:?} ({})", pattern, targets, file.display());
                    Some(PathAlias { prefix, suffix, targets })
                })
                .collect();
        }

        loaded.insert(file.to_path_buf(), Some(cfg.clone()));
        Some(cfg)
    }

    fn resolve_extends(&self, dir: &Path, ext: &str) -> Option<PathBuf> {
        let with_json = |p: PathBuf| -> Option<PathBuf> {
            if p.is_file() {
                return Some(p);
            }
            let json = PathBuf::from(format!("{}.json", p.display()));
            if json.is_file() {
                return Some(json);
            }
            let nested = p.join("tsconfig.json");
            nested.is_file().then_some(nested)
        };

        if ext.starts_with('.') || ext.starts_with('/') {
            return with_json(normalize(&dir.join(ext)));
        }

        // Package reference: look in node_modules, then in workspace packages
        let mut cur = Some(dir);
        while let Some(d) = cur {
            if let Some(p) = with_json(normalize(&d.join("node_modules").join(ext))) {
                return Some(p);
            }
            if d == self.root {
                break;
            }
            cur = d.parent();
        }
        let (name, sub) = split_package_request(ext);
        let pkg = self.workspace_package(name)?;
        with_json(if sub.is_empty() { pkg.to_path_buf() } else { normalize(&pkg.join(sub)) })
    }
}

/// Split `@scope/name/sub/path` into (`@scope/name`, `sub/path`).
pub fn split_package_request(request: &str) -> (&str, &str) {
    let mut idx = request.find('/');
    if request.starts_with('@')
        && let Some(first) = idx
    {
        idx = request[first + 1..].find('/').map(|i| first + 1 + i);
    }
    match idx {
        Some(i) => (&request[..i], &request[i + 1..]),
        None => (request, ""),
    }
}

/// Lexically normalize `.` and `..` components without touching the filesystem.
///
/// Import specifiers always use `/`, which Windows does not treat as a separator
/// inside verbatim (`\\?\`) paths, so it is converted first.
pub(crate) fn normalize(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    #[cfg(windows)]
    let s = s.replace('/', "\\");
    path_clean::clean(&*s)
}

fn discover_workspace_packages(root: &Path, package_dirs: &[PathBuf]) -> HashMap<String, PathBuf> {
    let patterns = workspace_patterns(root);
    let mut packages = HashMap::new();
    if patterns.is_empty() {
        return packages;
    }

    let build = |negated: bool| -> Option<GlobSet> {
        let mut b = GlobSetBuilder::new();
        for p in patterns.iter().filter(|p| p.starts_with('!') == negated) {
            let p = p.trim_start_matches('!').trim_start_matches("./").trim_end_matches('/');
            if let Ok(g) = GlobBuilder::new(p).literal_separator(true).build() {
                b.add(g);
            }
        }
        b.build().ok()
    };
    let (Some(include), Some(exclude)) = (build(false), build(true)) else {
        return packages;
    };

    for dir in package_dirs {
        let Ok(rel) = dir.strip_prefix(root) else { continue };
        if rel.as_os_str().is_empty() || !include.is_match(rel) || exclude.is_match(rel) {
            continue;
        }
        // Deno packages name themselves in deno.json, as JSR packages do.
        let name = ["package.json", "deno.json", "deno.jsonc"].iter().find_map(|file| {
            fs::read_to_string(dir.join(file))
                .ok()
                .and_then(|s| parse_jsonc(&s))
                .and_then(|v| v.get("name").and_then(|n| n.as_str()).map(str::to_string))
        });
        if let Some(name) = name {
            trace!("Workspace package '{}' at {}", name, dir.display());
            packages.entry(name).or_insert_with(|| dir.clone());
        }
    }
    packages
}

/// Workspace globs from pnpm-workspace.yaml and package.json `workspaces`
/// (npm, yarn and bun) and deno.json `workspace` (Deno), falling back to
/// lerna.json `packages`.
fn workspace_patterns(root: &Path) -> Vec<String> {
    let mut patterns = Vec::new();

    if let Ok(yaml) = fs::read_to_string(root.join("pnpm-workspace.yaml")) {
        let mut in_packages = false;
        for line in yaml.lines() {
            let trimmed = line.trim();
            if !line.starts_with(' ') && !line.starts_with('-') && !trimmed.is_empty() {
                in_packages = trimmed.starts_with("packages:");
                // Flow style: `packages: ['apps/*', 'packages/*']`
                if let Some(list) = trimmed
                    .strip_prefix("packages:")
                    .map(str::trim)
                    .and_then(|l| l.strip_prefix('['))
                    .and_then(|l| l.split(']').next())
                {
                    patterns.extend(
                        list.split(',')
                            .map(|i| i.trim().trim_matches(|c| c == '\'' || c == '"'))
                            .filter(|i| !i.is_empty())
                            .map(str::to_string),
                    );
                }
                continue;
            }
            if in_packages && let Some(item) = trimmed.strip_prefix('-') {
                let item = item.split(" #").next().unwrap_or("").trim();
                patterns.push(item.trim_matches(|c| c == '\'' || c == '"').to_string());
            }
        }
    }

    if let Ok(txt) = fs::read_to_string(root.join("package.json"))
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt)
    {
        let ws = v.get("workspaces");
        let list = ws
            .and_then(|w| w.as_array())
            .or_else(|| ws.and_then(|w| w.get("packages")).and_then(|p| p.as_array()));
        if let Some(list) = list {
            patterns.extend(list.iter().filter_map(|p| p.as_str()).map(str::to_string));
        }
    }

    // Deno: `"workspace": ["./a"]` or `"workspace": { "members": [...] }`.
    for file in ["deno.json", "deno.jsonc"] {
        if let Ok(txt) = fs::read_to_string(root.join(file))
            && let Some(v) = parse_jsonc(&txt)
            && let Some(ws) = v.get("workspace")
            && let Some(list) =
                ws.as_array().or_else(|| ws.get("members").and_then(|m| m.as_array()))
        {
            patterns.extend(list.iter().filter_map(|p| p.as_str()).map(str::to_string));
        }
    }

    // Lerna's own list, for repos that predate package manager workspaces
    if patterns.is_empty()
        && let Ok(txt) = fs::read_to_string(root.join("lerna.json"))
        && let Some(list) = parse_jsonc(&txt).and_then(|v| v.get("packages").cloned())
        && let Some(list) = list.as_array()
    {
        patterns.extend(list.iter().filter_map(|p| p.as_str()).map(str::to_string));
    }

    patterns
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(dir: &Path, path: &str, content: &str) -> PathBuf {
        let p = dir.join(path);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, content).unwrap();
        p
    }

    #[test]
    fn test_split_package_request() {
        assert_eq!(split_package_request("react"), ("react", ""));
        assert_eq!(split_package_request("lodash/debounce"), ("lodash", "debounce"));
        assert_eq!(split_package_request("@expo/config"), ("@expo/config", ""));
        assert_eq!(split_package_request("@expo/config/paths"), ("@expo/config", "paths"));
        assert_eq!(split_package_request("next/dist/a/b"), ("next", "dist/a/b"));
    }

    #[test]
    fn test_alias_exact_does_not_prefix_match() {
        let tmp = TempDir::new().unwrap();
        let root_buf = tmp.path().canonicalize().unwrap();
        let root = root_buf.as_path();
        write(root, "tsconfig.json", r#"{"compilerOptions":{"paths":{"utils":["./src/utils"]}}}"#);
        let p = Project::load(root);
        let cfg = p.tsconfig_for(&root.join("src/a.ts")).unwrap();
        assert_eq!(cfg.alias_candidates("utils").len(), 1);
        assert!(cfg.alias_candidates("utils-extra").is_empty());
    }

    #[test]
    fn test_alias_wildcard_longest_prefix_wins() {
        let tmp = TempDir::new().unwrap();
        let root_buf = tmp.path().canonicalize().unwrap();
        let root = root_buf.as_path();
        write(
            root,
            "tsconfig.json",
            r#"{"compilerOptions":{"baseUrl":".","paths":{"@/*":["src/*"],"@/components/*":["ui/*"]}}}"#,
        );
        let p = Project::load(root);
        let cfg = p.tsconfig_for(&root.join("a.ts")).unwrap();
        assert_eq!(cfg.alias_candidates("@/components/Button"), vec![root.join("ui/Button")]);
        assert_eq!(cfg.alias_candidates("@/lib/x"), vec![root.join("src/lib/x")]);
        // A bare "@" alias must not swallow scoped packages
        assert!(cfg.alias_candidates("@expo/config").is_empty());
    }

    #[test]
    fn test_nearest_tsconfig_wins() {
        let tmp = TempDir::new().unwrap();
        let root_buf = tmp.path().canonicalize().unwrap();
        let root = root_buf.as_path();
        write(root, "tsconfig.json", r#"{"compilerOptions":{"paths":{"~/*":["./root/*"]}}}"#);
        write(
            root,
            "packages/a/tsconfig.json",
            r#"{"compilerOptions":{"paths":{"~/*":["./src/*"]}}}"#,
        );
        let p = Project::load(root);
        let a = p.tsconfig_for(&root.join("packages/a/src/x.ts")).unwrap();
        assert_eq!(a.alias_candidates("~/y"), vec![root.join("packages/a/src/y")]);
        let r = p.tsconfig_for(&root.join("other/x.ts")).unwrap();
        assert_eq!(r.alias_candidates("~/y"), vec![root.join("root/y")]);
    }

    #[test]
    fn test_extends_inherits_paths_relative_to_parent() {
        let tmp = TempDir::new().unwrap();
        let root_buf = tmp.path().canonicalize().unwrap();
        let root = root_buf.as_path();
        write(
            root,
            "tsconfig.base.json",
            r#"{
              // comment
              "compilerOptions": { "baseUrl": ".", "paths": { "@lib/*": ["libs/*"], }, },
            }"#,
        );
        write(root, "apps/web/tsconfig.json", r#"{"extends":"../../tsconfig.base.json"}"#);
        let p = Project::load(root);
        let cfg = p.tsconfig_for(&root.join("apps/web/page.ts")).unwrap();
        assert_eq!(cfg.alias_candidates("@lib/x"), vec![root.join("libs/x")]);
    }

    #[test]
    fn test_pnpm_workspace_packages() {
        let tmp = TempDir::new().unwrap();
        let root_buf = tmp.path().canonicalize().unwrap();
        let root = root_buf.as_path();
        write(
            root,
            "pnpm-workspace.yaml",
            "packages:\n  - 'packages/*'\n  - \"!packages/ignored\"\n",
        );
        write(root, "package.json", r#"{"name":"root"}"#);
        write(root, "packages/a/package.json", r#"{"name":"@scope/a"}"#);
        write(root, "packages/ignored/package.json", r#"{"name":"ignored"}"#);
        write(root, "packages/a/nested/package.json", r#"{"name":"nested"}"#);
        let p = Project::load(root);
        assert_eq!(p.workspace_package("@scope/a"), Some(root.join("packages/a").as_path()));
        assert!(p.workspace_package("ignored").is_none());
        assert!(p.workspace_package("nested").is_none());
        assert!(p.workspace_package("root").is_none());
    }

    #[test]
    fn test_package_json_workspaces() {
        let tmp = TempDir::new().unwrap();
        let root_buf = tmp.path().canonicalize().unwrap();
        let root = root_buf.as_path();
        write(root, "package.json", r#"{"workspaces":{"packages":["apps/**"]}}"#);
        write(root, "apps/web/package.json", r#"{"name":"web"}"#);
        write(root, "apps/group/api/package.json", r#"{"name":"api"}"#);
        let p = Project::load(root);
        assert_eq!(p.workspace_package_count(), 2);
    }

    #[test]
    fn test_pnpm_flow_style_and_lerna() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        write(&root, "pnpm-workspace.yaml", "packages: ['apps/*', \"libs/*\"]\n");
        write(&root, "apps/web/package.json", r#"{"name":"web"}"#);
        write(&root, "libs/ui/package.json", r#"{"name":"@acme/ui"}"#);
        let p = Project::load(&root);
        assert_eq!(p.workspace_package_count(), 2);
        assert_eq!(p.package_for(&root.join("libs/ui/src/a.ts")), Some("@acme/ui"));
        assert_eq!(p.package_for(&root.join("other/a.ts")), None);

        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        write(&root, "lerna.json", r#"{ "packages": ["packages/*"], }"#);
        write(&root, "packages/a/package.json", r#"{"name":"a"}"#);
        write(&root, "packages/a/nested/package.json", r#"{"name":"nested"}"#);
        let p = Project::load(&root);
        assert_eq!(p.workspace_packages(), vec![("a", root.join("packages/a").as_path())]);
        // Only workspace members count, so the nested package.json is part of `a`.
        assert_eq!(p.package_for(&root.join("packages/a/nested/x.ts")), Some("a"));
    }

    #[test]
    fn test_deno_and_bun_workspaces() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        write(
            &root,
            "deno.jsonc",
            r#"{ // Deno
          "workspace": ["./packages/add", "./packages/cli"] }"#,
        );
        write(&root, "packages/add/deno.json", r#"{"name":"@acme/add","exports":"./mod.ts"}"#);
        write(&root, "packages/cli/deno.json", r#"{"name":"@acme/cli"}"#);
        write(&root, "packages/cli/package.json", r#"{"private":true}"#);
        let p = Project::load(&root);
        assert_eq!(
            p.workspace_packages(),
            vec![
                ("@acme/add", root.join("packages/add").as_path()),
                ("@acme/cli", root.join("packages/cli").as_path())
            ]
        );

        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        write(&root, "deno.json", r#"{"workspace":{"members":["libs/*"]}}"#);
        write(&root, "libs/x/deno.json", r#"{"name":"@acme/x"}"#);
        assert_eq!(Project::load(&root).workspace_package_count(), 1);

        // Bun reads package.json `workspaces`, as npm does.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        write(&root, "package.json", r#"{"workspaces":["packages/*"]}"#);
        write(&root, "bun.lock", "{}");
        write(&root, "packages/a/package.json", r#"{"name":"a"}"#);
        assert_eq!(Project::load(&root).workspace_package_count(), 1);
    }
}
