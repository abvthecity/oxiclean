//! Every tsconfig in a project, read the way `tsc` reads it: `extends`
//! followed and merged, paths resolved against the file that declares
//! them, and `include`, `exclude` and `files` expanded to the files the
//! program starts from.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    path::{Path, PathBuf},
};

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use ignore::WalkBuilder;
use oxiclean_core::{
    Project,
    jsonc_tree::{JsoncDoc, Node, Segment},
    split_package_request,
};
use serde_json::{Map, Value};
use tracing::{debug, trace, warn};

/// How deep `extends` chains are followed, so a cycle ends.
pub(crate) const MAX_EXTENDS_DEPTH: usize = 16;

/// Compiler options that hold one path, resolved against the config that
/// sets them.
const PATH_OPTIONS: &[&str] =
    &["outDir", "rootDir", "declarationDir", "baseUrl", "tsBuildInfoFile"];

/// Compiler options that hold a list of paths.
const PATH_LIST_OPTIONS: &[&str] = &["typeRoots", "rootDirs"];

/// Directories wildcards never enter, as in `tsc`.
const SKIPPED_DIRS: &[&str] = &["node_modules", "bower_components", "jspm_packages"];

/// Directories of test inputs and expected outputs: the tsconfigs in them
/// are data for a tool's tests, not programs anyone runs, and editing them
/// would change what the tests compare.
const TEST_INPUT_DIRS: &[&str] = &[
    "fixtures",
    "fixture",
    "__fixtures__",
    "testdata",
    "goldens",
    "test-goldens",
    "test-output",
    "__snapshots__",
];

/// TypeScript's own extensions, in `tsc`'s priority order.
const TS_EXTENSIONS: &[&str] = &[".ts", ".tsx", ".d.ts", ".cts", ".d.cts", ".mts", ".d.mts"];

/// The extensions `allowJs` adds.
const JS_EXTENSIONS: &[&str] = &[".js", ".jsx", ".cjs", ".mjs"];

/// A value from a config, with the directory of the file that set it,
/// which relative paths in it are resolved against.
#[derive(Debug, Clone)]
struct Declared<T> {
    value: T,
    dir: PathBuf,
}

/// One tsconfig file, with what it inherits.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct TsConfigFile {
    /// Absolute path of the file.
    pub path: PathBuf,
    /// Its directory.
    pub dir: PathBuf,
    /// Its own text, for lines and fixes.
    pub doc: JsoncDoc,
    /// The configs it extends directly, resolved.
    pub extends: Vec<PathBuf>,
    /// Every config it extends, directly or not.
    pub ancestors: BTreeSet<PathBuf>,
    /// Whether a config in its `extends` chain couldn't be found, so its
    /// options are only partly known.
    pub partial: bool,
    /// Merged `compilerOptions`, with path options and `paths` targets made
    /// absolute.
    pub options: Map<String, Value>,
    /// Merged `include`, as absolute patterns, if any config set it.
    pub include: Option<Vec<String>>,
    /// Merged `exclude`, as absolute patterns, if any config set it.
    pub exclude: Option<Vec<String>>,
    /// Merged `files`, as absolute paths, if any config set them.
    pub files: Option<Vec<PathBuf>>,
    /// This file's own `references`, which aren't inherited.
    pub references: Vec<Reference>,
    /// Whether `tsc` or an editor runs it as a program, rather than it
    /// only being extended by others.
    pub is_program: bool,
    /// The files the program starts from, sorted.
    pub root_files: Vec<PathBuf>,
}

/// One entry of a tsconfig's `references`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Reference {
    /// The `path` as written.
    pub raw: String,
    /// The tsconfig it points at: the path itself when it names a file,
    /// else its `tsconfig.json`.
    pub target: PathBuf,
    /// 1-based line of the entry.
    pub line: u32,
    /// The entry as JSON, to remove it by.
    pub value: Value,
}

impl TsConfigFile {
    /// A boolean compiler option, if set.
    #[must_use]
    pub fn flag(&self, name: &str) -> Option<bool> {
        self.options.get(name).and_then(Value::as_bool)
    }

    /// A path compiler option, if set.
    #[must_use]
    pub fn path_option(&self, name: &str) -> Option<PathBuf> {
        self.options.get(name).and_then(Value::as_str).map(PathBuf::from)
    }

    /// 1-based line of a top-level key of this file, or 0 when it doesn't
    /// set it.
    #[must_use]
    pub fn line_of(&self, path: &[&str]) -> u32 {
        let segs: Vec<Segment> = path.iter().map(|k| Segment::from(*k)).collect();
        self.doc.line_of(&segs)
    }

    /// Whether this file itself sets the top-level `key`, rather than
    /// inheriting it.
    #[must_use]
    pub fn sets(&self, key: &str) -> bool {
        self.doc.root.get(key).is_some()
    }

    /// Whether this file itself sets compiler option `name`.
    #[must_use]
    pub fn sets_option(&self, name: &str) -> bool {
        self.doc.root.get("compilerOptions").and_then(|o| o.get(name)).is_some()
    }

    /// The file's name, as `tsconfig.build.json`.
    #[must_use]
    pub fn name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    /// The output directories this config emits to: `outDir` and
    /// `declarationDir`, unless they hold the config itself.
    #[must_use]
    pub fn output_dirs(&self) -> Vec<PathBuf> {
        ["outDir", "declarationDir"]
            .iter()
            .filter_map(|o| self.path_option(o))
            .filter(|d| !self.dir.starts_with(d))
            .collect()
    }
}

/// Every tsconfig in a project.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct TsConfigs {
    pub configs: Vec<TsConfigFile>,
}

impl TsConfigs {
    /// Finds and reads every `tsconfig*.json` and `jsconfig*.json` under
    /// the project root that git doesn't ignore, outside test fixture
    /// directories, and the configs they extend in the repo, and expands
    /// each program's files.
    #[must_use]
    pub fn load(project: &Project) -> Self {
        let root = &project.root;
        let paths = find_configs(root);
        debug!("Found {} tsconfig files", paths.len());
        let mut raw: HashMap<PathBuf, Option<Raw>> = HashMap::new();
        for p in &paths {
            read_raw(project, p, &mut raw, 0);
        }
        // Bases in the repo with other names, such as a shared
        // `@repo/tsconfig/base.json`, so fixes can go where they're set.
        let found: BTreeSet<&PathBuf> = paths.iter().collect();
        let mut bases: Vec<PathBuf> = raw
            .keys()
            .filter(|p| {
                !found.contains(p)
                    && p.strip_prefix(root).is_ok_and(|rel| {
                        !rel.components().any(|c| SKIPPED_DIRS.iter().any(|s| c.as_os_str() == *s))
                    })
            })
            .cloned()
            .collect();
        bases.sort();
        let mut configs: Vec<TsConfigFile> =
            paths.iter().chain(&bases).filter_map(|p| finish(p, &raw)).collect();

        let extended: BTreeSet<PathBuf> =
            configs.iter().flat_map(|c| c.extends.iter().cloned()).collect();
        let referenced: BTreeSet<PathBuf> =
            configs.iter().flat_map(|c| c.references.iter().map(|r| r.target.clone())).collect();
        let in_scripts = configs_in_scripts(project, &configs);
        let files = SourceFiles::walk(root);
        for c in &mut configs {
            let name = c.name();
            c.is_program = name == "tsconfig.json"
                || name == "jsconfig.json"
                || referenced.contains(&c.path)
                || in_scripts.contains(&c.path)
                || !extended.contains(&c.path);
            if c.is_program {
                c.root_files = files.matching(c);
            }
        }
        Self { configs }
    }

    /// The programs: configs `tsc` or an editor runs.
    pub fn programs(&self) -> impl Iterator<Item = &TsConfigFile> {
        self.configs.iter().filter(|c| c.is_program)
    }

    /// The config at `path`.
    #[must_use]
    pub fn get(&self, path: &Path) -> Option<&TsConfigFile> {
        self.configs.iter().find(|c| c.path == path)
    }
}

/// The paths of every tsconfig git doesn't ignore, sorted.
fn find_configs(root: &Path) -> Vec<PathBuf> {
    let walker = WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .filter_entry(|e| {
            let n = e.file_name();
            n != "node_modules"
                && n != ".git"
                && (e.depth() == 0 || !TEST_INPUT_DIRS.iter().any(|d| n == *d))
        })
        .build();
    let mut out: Vec<PathBuf> = walker
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .filter(|e| {
            let name = e.file_name().to_string_lossy();
            (name.starts_with("tsconfig") || name.starts_with("jsconfig"))
                && name.ends_with(".json")
        })
        .map(ignore::DirEntry::into_path)
        .collect();
    out.sort();
    out
}

/// What one file says, before merging.
#[derive(Debug, Clone)]
struct Raw {
    doc: JsoncDoc,
    extends: Vec<PathBuf>,
    /// Whether an `extends` entry couldn't be found, such as a package
    /// that isn't installed.
    unresolved: bool,
}

/// Reads `path` and, recursively, what it extends into `raw`.
fn read_raw(project: &Project, path: &Path, raw: &mut HashMap<PathBuf, Option<Raw>>, depth: usize) {
    if raw.contains_key(path) || depth > MAX_EXTENDS_DEPTH {
        return;
    }
    raw.insert(path.to_path_buf(), None);
    let Some(doc) = fs::read_to_string(path).ok().and_then(JsoncDoc::parse) else {
        warn!("Skipping {}: not valid JSON", path.display());
        return;
    };
    let dir = path.parent().unwrap_or(Path::new(""));
    let names: Vec<String> = match doc.root.get("extends").and_then(|n| doc.value(n)) {
        Some(Value::String(s)) => vec![s],
        Some(Value::Array(a)) => {
            a.into_iter().filter_map(|v| v.as_str().map(String::from)).collect()
        }
        _ => Vec::new(),
    };
    let extends: Vec<PathBuf> = names
        .iter()
        .filter_map(|n| {
            // Canonical, so a workspace package's base reached through a
            // `node_modules` link is the file in the repo.
            let r = resolve_extends(project, dir, n).map(|p| fs::canonicalize(&p).unwrap_or(p));
            if r.is_none() {
                trace!("Could not resolve extends '{n}' in {}", path.display());
            }
            r
        })
        .collect();
    for e in &extends {
        read_raw(project, e, raw, depth + 1);
    }
    let unresolved = extends.len() < names.len();
    raw.insert(path.to_path_buf(), Some(Raw { doc, extends, unresolved }));
}

/// The tsconfig `extends` names from `dir`: a path, or a package's
/// `tsconfig.json` or a file in it, looked up in `node_modules` and then
/// among the workspace packages.
fn resolve_extends(project: &Project, dir: &Path, name: &str) -> Option<PathBuf> {
    let file = |p: PathBuf| -> Option<PathBuf> {
        if p.is_file() {
            return Some(p);
        }
        let mut json = p.clone().into_os_string();
        json.push(".json");
        let json = PathBuf::from(json);
        if json.is_file() {
            return Some(json);
        }
        let nested = p.join("tsconfig.json");
        nested.is_file().then_some(nested)
    };
    if name.starts_with('.') || Path::new(name).is_absolute() {
        return file(path_clean::clean(dir.join(name)));
    }
    for d in dir.ancestors() {
        if let Some(p) = file(d.join("node_modules").join(name)) {
            return Some(p);
        }
        if d == project.root {
            break;
        }
    }
    let (pkg, sub) = split_package_request(name);
    let pkg_dir = project.workspace_package(pkg)?;
    file(if sub.is_empty() { pkg_dir.to_path_buf() } else { pkg_dir.join(sub) })
}

/// Merges `path` with what it extends.
fn finish(path: &Path, raw: &HashMap<PathBuf, Option<Raw>>) -> Option<TsConfigFile> {
    let own = raw.get(path)?.clone()?;
    let dir = path.parent()?.to_path_buf();
    let mut merged = Merged::default();
    merge(path, raw, &mut merged, &mut BTreeSet::new(), 0);
    let mut ancestors = BTreeSet::new();
    collect_ancestors(path, raw, &mut ancestors, 0);
    // Unknown when an `extends` isn't installed, isn't valid JSON, or is
    // past the depth limit.
    let partial = own.unresolved
        || ancestors.iter().any(|a| match raw.get(a) {
            Some(Some(r)) => r.unresolved,
            _ => true,
        });

    let config_dir = dir.to_string_lossy().into_owned();
    let resolve = |d: &Declared<Value>| -> Value {
        let s = |v: &str| -> Value {
            let v = v.replace("${configDir}", &config_dir);
            Value::String(path_clean::clean(d.dir.join(v)).to_string_lossy().into_owned())
        };
        match &d.value {
            Value::String(v) => s(v),
            Value::Array(items) => {
                Value::Array(items.iter().map(|i| i.as_str().map_or(i.clone(), s)).collect())
            }
            other => other.clone(),
        }
    };
    let mut options = Map::new();
    for (k, d) in &merged.options {
        let is_path = PATH_OPTIONS.contains(&k.as_str()) || PATH_LIST_OPTIONS.contains(&k.as_str());
        options.insert(k.clone(), if is_path { resolve(d) } else { d.value.clone() });
    }
    // `paths` targets are relative to `baseUrl`, or else to the config that
    // sets `paths`.
    if let Some(d) = merged.options.get("paths")
        && let Value::Object(map) = &d.value
    {
        let base = options
            .get("baseUrl")
            .and_then(Value::as_str)
            .map_or_else(|| d.dir.clone(), PathBuf::from);
        let resolved: Map<String, Value> = map
            .iter()
            .map(|(k, targets)| {
                let targets = targets
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(|t| {
                        let t = t.replace("${configDir}", &config_dir);
                        // A path, not a glob: `slashes` would break a Windows
                        // verbatim path (`\\?\C:\`).
                        Value::String(
                            path_clean::clean(base.join(t)).to_string_lossy().into_owned(),
                        )
                    })
                    .collect();
                (k.clone(), Value::Array(targets))
            })
            .collect();
        options.insert("paths".into(), Value::Object(resolved));
    }
    let paths = |d: &Option<Declared<Vec<String>>>| {
        d.as_ref().map(|d| {
            d.value
                .iter()
                .map(|p| path_clean::clean(d.dir.join(p.replace("${configDir}", &config_dir))))
                .collect::<Vec<_>>()
        })
    };
    let patterns = |d: &Option<Declared<Vec<String>>>| {
        paths(d).map(|p| p.iter().map(|p| slashes(p)).collect::<Vec<_>>())
    };
    let references = own
        .doc
        .root
        .get("references")
        .map(|refs| refs.items().iter().filter_map(|n| reference(&own.doc, &dir, n)).collect())
        .unwrap_or_default();
    Some(TsConfigFile {
        path: path.to_path_buf(),
        dir,
        extends: own.extends,
        ancestors,
        partial,
        options,
        include: patterns(&merged.include),
        exclude: patterns(&merged.exclude),
        files: paths(&merged.files),
        references,
        is_program: false,
        root_files: Vec::new(),
        doc: own.doc,
    })
}

fn reference(doc: &JsoncDoc, dir: &Path, node: &Node) -> Option<Reference> {
    let raw = node.get("path").and_then(|p| doc.value(p))?.as_str()?.to_string();
    let target = path_clean::clean(dir.join(&raw));
    let target = if target.extension().is_some_and(|e| e == "json") {
        target
    } else {
        target.join("tsconfig.json")
    };
    let value = doc.value(node)?;
    Some(Reference { raw, target, line: doc.line(node.span.start), value })
}

/// Every config `path` extends, directly or not.
fn collect_ancestors(
    path: &Path,
    raw: &HashMap<PathBuf, Option<Raw>>,
    out: &mut BTreeSet<PathBuf>,
    depth: usize,
) {
    let Some(Some(r)) = raw.get(path) else { return };
    if depth > MAX_EXTENDS_DEPTH {
        return;
    }
    for e in &r.extends {
        if out.insert(e.clone()) {
            collect_ancestors(e, raw, out, depth + 1);
        }
    }
}

/// Settings merged along an `extends` chain.
#[derive(Debug, Default)]
struct Merged {
    options: BTreeMap<String, Declared<Value>>,
    include: Option<Declared<Vec<String>>>,
    exclude: Option<Declared<Vec<String>>>,
    files: Option<Declared<Vec<String>>>,
}

/// Merges what `path` extends, in order, then `path` itself, into `out`.
fn merge(
    path: &Path,
    raw: &HashMap<PathBuf, Option<Raw>>,
    out: &mut Merged,
    seen: &mut BTreeSet<PathBuf>,
    depth: usize,
) {
    // `seen` holds the chain being merged, so a cycle ends while a base two
    // siblings share is applied for each, as `tsc` does.
    if depth > MAX_EXTENDS_DEPTH || !seen.insert(path.to_path_buf()) {
        return;
    }
    let Some(Some(r)) = raw.get(path) else {
        seen.remove(path);
        return;
    };
    for e in &r.extends {
        merge(e, raw, out, seen, depth + 1);
    }
    seen.remove(path);
    let dir = path.parent().unwrap_or(Path::new("")).to_path_buf();
    let root = &r.doc.root;
    if let Some(opts) = root.get("compilerOptions") {
        for m in opts.members() {
            if let Some(value) = r.doc.value(&m.value) {
                out.options.insert(m.key.clone(), Declared { value, dir: dir.clone() });
            }
        }
    }
    let list = |key: &str| -> Option<Declared<Vec<String>>> {
        let v = r.doc.value(root.get(key)?)?;
        let value = v.as_array()?.iter().filter_map(|s| s.as_str().map(String::from)).collect();
        Some(Declared { value, dir: dir.clone() })
    };
    if let Some(v) = list("include") {
        out.include = Some(v);
    }
    if let Some(v) = list("exclude") {
        out.exclude = Some(v);
    }
    if let Some(v) = list("files") {
        out.files = Some(v);
    }
}

/// The tsconfigs package.json scripts name with `-p`, `--project` or
/// `-b`/`--build`.
fn configs_in_scripts(project: &Project, configs: &[TsConfigFile]) -> BTreeSet<PathBuf> {
    let known: BTreeSet<&Path> = configs.iter().map(|c| c.path.as_path()).collect();
    let mut dirs = vec![project.root.clone()];
    dirs.extend(project.workspace_packages().into_iter().map(|(_, d)| d.to_path_buf()));
    let mut out = BTreeSet::new();
    for dir in dirs {
        let Some(pkg) = project.package_json(&dir) else { continue };
        let Some(scripts) = pkg.get("scripts").and_then(Value::as_object) else { continue };
        for script in scripts.values().filter_map(Value::as_str) {
            for word in script.split_whitespace() {
                let word = word.trim_matches(|c| c == '"' || c == '\'');
                if word.ends_with(".json") {
                    let p = path_clean::clean(dir.join(word));
                    if known.contains(p.as_path()) {
                        out.insert(p);
                    }
                }
            }
        }
    }
    out
}

/// Every TS and JS file under the root, found once and shared by every
/// config's expansion. Unlike the module graph, this walk doesn't honour
/// `.gitignore`: `tsc` doesn't either, which is how build output ends up
/// in a program.
struct SourceFiles {
    /// Sorted, so the files under a directory are one range.
    files: Vec<PathBuf>,
}

impl SourceFiles {
    fn walk(root: &Path) -> Self {
        let walker = WalkBuilder::new(root)
            .standard_filters(false)
            .filter_entry(|e| {
                let n = e.file_name();
                n != ".git" && !SKIPPED_DIRS.iter().any(|s| n == *s)
            })
            .build();
        let mut files: Vec<PathBuf> = walker
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
            .map(ignore::DirEntry::into_path)
            .filter(|p| extension_of(p).is_some())
            .collect();
        files.sort();
        Self { files }
    }

    /// The files under `dir`.
    fn under(&self, dir: &Path) -> &[PathBuf] {
        let start = self.files.partition_point(|f| f.as_path() < dir);
        let end = start + self.files[start..].partition_point(|f| f.starts_with(dir));
        &self.files[start..end]
    }

    /// The root files of `config`'s program, as `tsc` would list them.
    fn matching(&self, config: &TsConfigFile) -> Vec<PathBuf> {
        let allow_js = config.flag("allowJs").unwrap_or(false);
        let supported = |p: &Path| {
            extension_of(p).is_some_and(|e| {
                TS_EXTENSIONS.contains(&e) || (allow_js && JS_EXTENSIONS.contains(&e))
            })
        };
        let include = match (&config.include, &config.files) {
            (Some(i), _) => i.clone(),
            (None, Some(_)) => Vec::new(),
            (None, None) => vec![format!("{}/**/*", slashes(&config.dir))],
        };
        let exclude = config.exclude.clone().unwrap_or_else(|| default_exclude(config));
        let exclude_set = globs(&exclude, true);
        let mut wildcard: BTreeSet<PathBuf> = BTreeSet::new();
        for pattern in &include {
            let pattern = as_files_pattern(pattern);
            let Some(set) = globs(std::slice::from_ref(&pattern), false) else { continue };
            let (base, rest) = split_base(&pattern);
            // Wildcards skip hidden files unless the pattern names them,
            // and `*` never matches a name ending in `.min.js`.
            let explicit_hidden = rest.contains("/.") || rest.starts_with('.');
            let explicit_min = rest.ends_with(".min.js") || !rest.contains(['*', '?']);
            for f in self.under(&base) {
                let rel = f.strip_prefix(&base).unwrap_or(f);
                let hidden =
                    rel.components().any(|c| c.as_os_str().to_string_lossy().starts_with('.'));
                let min = f.file_name().is_some_and(|n| n.to_string_lossy().ends_with(".min.js"));
                if (hidden && !explicit_hidden) || (min && !explicit_min) || !supported(f) {
                    continue;
                }
                let text = slashes(f);
                if set.is_match(&text) && !exclude_set.as_ref().is_some_and(|e| e.is_match(&text)) {
                    wildcard.insert(f.clone());
                }
            }
        }
        // Of files that differ only in extension, `tsc` keeps the one
        // whose extension comes first in its group: `a.d.ts` and `a.js` next
        // to `a.ts` are its output.
        let mut best: BTreeMap<(String, usize), usize> = BTreeMap::new();
        for f in &wildcard {
            if let Some((group, rank)) = extension_rank(f) {
                let e = best.entry((stem(f), group)).or_insert(rank);
                *e = (*e).min(rank);
            }
        }
        let mut out: BTreeSet<PathBuf> = wildcard
            .into_iter()
            .filter(|f| {
                extension_rank(f)
                    .is_none_or(|(group, rank)| best.get(&(stem(f), group)) == Some(&rank))
            })
            .collect();
        if let Some(files) = &config.files {
            out.extend(files.iter().filter(|f| f.is_file()).cloned());
        }
        out.into_iter().collect()
    }
}

/// What `tsc` excludes when a config sets no `exclude`: package folders,
/// and its `outDir` and `declarationDir`.
fn default_exclude(config: &TsConfigFile) -> Vec<String> {
    let mut d: Vec<String> =
        SKIPPED_DIRS.iter().map(|s| format!("{}/{s}", slashes(&config.dir))).collect();
    d.extend(
        ["outDir", "declarationDir"]
            .iter()
            .filter_map(|o| config.path_option(o))
            .map(|o| slashes(&o)),
    );
    d
}

/// `tsc`'s extension groups, each in priority order. Of files with one
/// stem, only the first extension of each group is kept.
const EXTENSION_GROUPS: &[&[&str]] = &[
    &[".ts", ".tsx", ".d.ts", ".js", ".jsx"],
    &[".cts", ".d.cts", ".cjs"],
    &[".mts", ".d.mts", ".mjs"],
];

/// The group and rank of `f`'s extension in [`EXTENSION_GROUPS`].
fn extension_rank(f: &Path) -> Option<(usize, usize)> {
    let ext = extension_of(f)?;
    EXTENSION_GROUPS
        .iter()
        .enumerate()
        .find_map(|(g, exts)| exts.iter().position(|e| *e == ext).map(|r| (g, r)))
}

/// The directory of `pattern` before its first wildcard (for a pattern
/// naming one file, its directory), and the rest of the pattern.
fn split_base(pattern: &str) -> (PathBuf, &str) {
    let prefix = verbatim_prefix_len(pattern);
    let literal_end = pattern[prefix..].find(['*', '?']).map_or(pattern.len(), |i| i + prefix);
    let cut = pattern[..literal_end].rfind('/').unwrap_or(0);
    let base = &pattern[..cut];
    // Patterns use `/`; the platform's separator keeps Windows prefixes
    // such as `\\?\C:\` intact.
    (PathBuf::from(base.replace('/', std::path::MAIN_SEPARATOR_STR)), &pattern[cut..])
}

/// `pattern` as a pattern of files: one whose last part names no file
/// (no `.` and no wildcard) is a directory, and means everything in it.
fn as_files_pattern(pattern: &str) -> String {
    let last = pattern.rsplit('/').next().unwrap_or(pattern);
    if last.contains(['.', '*', '?']) { pattern.to_string() } else { format!("{pattern}/**/*") }
}

/// A glob set of `patterns`, with `tsc`'s syntax: only `*`, `?` and `**`
/// are special. Exclude patterns also match everything under a directory
/// they name.
fn globs(patterns: &[String], directories_too: bool) -> Option<GlobSet> {
    let mut b = GlobSetBuilder::new();
    for p in patterns {
        let prefix = verbatim_prefix_len(p);
        let rest: String = p[prefix..]
            .chars()
            .flat_map(|c| match c {
                '[' | ']' | '{' | '}' => vec!['[', c, ']'],
                c => vec![c],
            })
            .collect();
        let escaped = if prefix > 0 { format!("//[?]/{rest}") } else { rest };
        let mut add = |g: &str| match GlobBuilder::new(g).literal_separator(true).build() {
            Ok(glob) => {
                b.add(glob);
            }
            Err(e) => warn!("Skipping tsconfig pattern '{p}': {e}"),
        };
        add(&escaped);
        if directories_too {
            add(&format!("{}/**", escaped.trim_end_matches('/')));
        }
    }
    b.build().ok()
}

/// The length of a Windows verbatim prefix (`//?/`, as `\\\\?\\` reads with
/// `/` separators) at the start of `pattern`, whose `?` isn't a wildcard.
fn verbatim_prefix_len(pattern: &str) -> usize {
    if pattern.starts_with("//?/") { 4 } else { 0 }
}

/// The TS or JS extension of `p`, longest first: `.d.ts` before `.ts`.
fn extension_of(p: &Path) -> Option<&'static str> {
    let name = p.file_name()?.to_str()?;
    [".d.ts", ".d.mts", ".d.cts"]
        .into_iter()
        .chain(TS_EXTENSIONS.iter().copied())
        .chain(JS_EXTENSIONS.iter().copied())
        .find(|e| name.ends_with(e) && name.len() > e.len())
}

/// `p` without its TS or JS extension.
fn stem(p: &Path) -> String {
    let s = slashes(p);
    match extension_of(p) {
        Some(e) => s[..s.len() - e.len()].to_string(),
        None => s,
    }
}

/// `p` with `/` separators.
pub(crate) fn slashes(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// `to` relative to directory `from`, with `/` separators, as tsconfigs
/// write paths: `dist`, `../shared`.
#[must_use]
pub fn relative(from: &Path, to: &Path) -> String {
    let from: Vec<_> = from.components().collect();
    let to: Vec<_> = to.components().collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<String> = vec!["..".to_string(); from.len() - common];
    parts.extend(to[common..].iter().map(|c| c.as_os_str().to_string_lossy().into_owned()));
    if parts.is_empty() { ".".to_string() } else { parts.join("/") }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(root: &Path, path: &str, text: &str) {
        let p = root.join(path);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    fn rel(root: &Path, files: &[PathBuf]) -> Vec<String> {
        files.iter().map(|f| slashes(f.strip_prefix(root).unwrap())).collect()
    }

    #[test]
    fn expands_like_tsc() {
        let t = TempDir::new().unwrap();
        let root = t.path().canonicalize().unwrap();
        write(
            &root,
            "tsconfig.base.json",
            r#"{ "compilerOptions": { "outDir": "${configDir}/out" }, "exclude": ["**/*.spec.ts"] }"#,
        );
        write(
            &root,
            "pkg/tsconfig.json",
            r#"{ "extends": "../tsconfig.base", "include": ["src", "types/*.d.ts"] }"#,
        );
        for f in [
            "pkg/src/a.ts",
            "pkg/src/a.js",
            "pkg/src/a.d.ts",
            "pkg/src/b.tsx",
            "pkg/src/c.spec.ts",
            "pkg/src/.hidden/x.ts",
            "pkg/src/node_modules/m.ts",
            "pkg/src/d.js",
            "pkg/types/g.d.ts",
            "pkg/types/nested/h.d.ts",
            "pkg/other.ts",
        ] {
            write(&root, f, "");
        }
        let project = Project::load(&root);
        let configs = TsConfigs::load(&project);
        let c = configs.get(&root.join("pkg/tsconfig.json")).unwrap();
        assert!(c.is_program);
        assert_eq!(c.path_option("outDir"), Some(root.join("pkg/out")));
        assert_eq!(c.extends, [root.join("tsconfig.base.json")]);
        assert_eq!(
            rel(&root, &c.root_files),
            ["pkg/src/a.ts", "pkg/src/b.tsx", "pkg/types/g.d.ts"]
        );
        // Only extended, so not a program.
        assert!(!configs.get(&root.join("tsconfig.base.json")).unwrap().is_program);
    }

    #[test]
    fn default_include_files_and_references() {
        let t = TempDir::new().unwrap();
        let root = t.path().canonicalize().unwrap();
        write(
            &root,
            "tsconfig.json",
            r#"{ "files": [], "references": [{ "path": "./a" }, { "path": "./b/tsconfig.lib.json" }] }"#,
        );
        write(
            &root,
            "a/tsconfig.json",
            r#"{ "compilerOptions": { "allowJs": true, "outDir": "dist" } }"#,
        );
        write(&root, "b/tsconfig.lib.json", r#"{ "files": ["main.ts"] }"#);
        for f in ["a/x.ts", "a/y.js", "a/dist/x.js", "b/main.ts", "b/other.ts"] {
            write(&root, f, "");
        }
        let configs = TsConfigs::load(&Project::load(&root));
        let solution = configs.get(&root.join("tsconfig.json")).unwrap();
        assert!(solution.root_files.is_empty());
        assert_eq!(
            solution.references.iter().map(|r| r.target.clone()).collect::<Vec<_>>(),
            [root.join("a/tsconfig.json"), root.join("b/tsconfig.lib.json")]
        );
        assert_eq!(solution.references[1].line, 1);
        let a = configs.get(&root.join("a/tsconfig.json")).unwrap();
        assert_eq!(rel(&root, &a.root_files), ["a/x.ts", "a/y.js"]);
        let b = configs.get(&root.join("b/tsconfig.lib.json")).unwrap();
        assert!(b.is_program);
        assert_eq!(rel(&root, &b.root_files), ["b/main.ts"]);
    }

    #[test]
    fn minified_declaration_dir_and_extension_groups() {
        let t = TempDir::new().unwrap();
        let root = t.path().canonicalize().unwrap();
        write(
            &root,
            "tsconfig.json",
            r#"{ "compilerOptions": { "allowJs": true, "declarationDir": "types" } }"#,
        );
        for f in [
            "vendor/j.min.js",
            "vendor/k.min.mjs",
            "types/a.d.ts",
            "a.ts",
            "a.tsx",
            "c.cts",
            "c.cjs",
            "d.mts",
            "d.mjs",
            "e.js",
            "e.jsx",
        ] {
            write(&root, f, "");
        }
        let configs = TsConfigs::load(&Project::load(&root));
        let c = configs.get(&root.join("tsconfig.json")).unwrap();
        assert_eq!(
            rel(&root, &c.root_files),
            ["a.ts", "c.cts", "d.mts", "e.js", "vendor/k.min.mjs"]
        );
    }

    #[test]
    fn extends_siblings_each_apply_their_base() {
        let t = TempDir::new().unwrap();
        let root = t.path().canonicalize().unwrap();
        write(&root, "tsconfig.base.json", r#"{ "compilerOptions": { "outDir": "out" } }"#);
        write(
            &root,
            "tsconfig.a.json",
            r#"{ "extends": "./tsconfig.base.json", "compilerOptions": { "outDir": "build" } }"#,
        );
        write(&root, "tsconfig.b.json", r#"{ "extends": "./tsconfig.base.json" }"#);
        write(
            &root,
            "tsconfig.json",
            r#"{ "extends": ["./tsconfig.a.json", "./tsconfig.b.json"] }"#,
        );
        let configs = TsConfigs::load(&Project::load(&root));
        let c = configs.get(&root.join("tsconfig.json")).unwrap();
        assert_eq!(c.path_option("outDir"), Some(root.join("out")));
        assert_eq!(c.ancestors.len(), 3);
    }

    #[test]
    fn relative_paths() {
        assert_eq!(relative(Path::new("/a/b"), Path::new("/a/b/dist")), "dist");
        assert_eq!(relative(Path::new("/a/b"), Path::new("/a/c")), "../c");
        assert_eq!(relative(Path::new("/a/b"), Path::new("/a/b")), ".");
    }
}
