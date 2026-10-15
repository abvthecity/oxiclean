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
    jsonc_tree::{JsoncDoc, Segment},
    split_package_request,
};
use serde_json::{Map, Value};
use tracing::{debug, trace, warn};

/// How deep `extends` chains are followed, so a cycle ends.
const MAX_EXTENDS_DEPTH: usize = 16;

/// Compiler options that hold one path, resolved against the config that
/// sets them.
const PATH_OPTIONS: &[&str] =
    &["outDir", "rootDir", "declarationDir", "baseUrl", "tsBuildInfoFile"];

/// Compiler options that hold a list of paths.
const PATH_LIST_OPTIONS: &[&str] = &["typeRoots", "rootDirs"];

/// Directories wildcards never enter, as in `tsc`.
const SKIPPED_DIRS: &[&str] = &["node_modules", "bower_components", "jspm_packages"];

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
    /// Merged `compilerOptions`, with path options made absolute.
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
pub struct Reference {
    /// The `path` as written.
    pub raw: String,
    /// The tsconfig it points at: the path itself when it names a file,
    /// else its `tsconfig.json`.
    pub target: PathBuf,
    /// 1-based line of the entry.
    pub line: u32,
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
pub struct TsConfigs {
    pub configs: Vec<TsConfigFile>,
}

impl TsConfigs {
    /// Finds and reads every `tsconfig*.json` and `jsconfig*.json` under
    /// the project root that git doesn't ignore, and expands each
    /// program's files.
    #[must_use]
    pub fn load(project: &Project) -> Self {
        let root = &project.root;
        let paths = find_configs(root);
        debug!("Found {} tsconfig files", paths.len());
        let mut raw: HashMap<PathBuf, Option<Raw>> = HashMap::new();
        for p in &paths {
            read_raw(project, p, &mut raw, 0);
        }
        let mut configs: Vec<TsConfigFile> = paths.iter().filter_map(|p| finish(p, &raw)).collect();

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
        .filter_entry(|e| e.file_name() != "node_modules" && e.file_name() != ".git")
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
            let r = resolve_extends(project, dir, n);
            if r.is_none() {
                trace!("Could not resolve extends '{n}' in {}", path.display());
            }
            r
        })
        .collect();
    for e in &extends {
        read_raw(project, e, raw, depth + 1);
    }
    raw.insert(path.to_path_buf(), Some(Raw { doc, extends }));
}

/// The tsconfig `extends` names from `dir`: a path, or a package's
/// `tsconfig.json` or a file in it, looked up in `node_modules` and then
/// among the workspace packages.
fn resolve_extends(project: &Project, dir: &Path, name: &str) -> Option<PathBuf> {
    let file = |p: PathBuf| -> Option<PathBuf> {
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
    let mut ancestors = BTreeSet::new();
    merge(path, raw, &mut merged, &mut ancestors, 0);
    ancestors.remove(path);

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
    let patterns = |d: &Option<Declared<Vec<String>>>| {
        d.as_ref().map(|d| {
            d.value
                .iter()
                .map(|p| {
                    let p = p.replace("${configDir}", &config_dir);
                    slashes(&path_clean::clean(d.dir.join(p)))
                })
                .collect::<Vec<_>>()
        })
    };
    let references = own
        .doc
        .root
        .get("references")
        .map(|refs| {
            refs.items()
                .iter()
                .enumerate()
                .filter_map(|(i, n)| reference(&own.doc, &dir, i, n))
                .collect()
        })
        .unwrap_or_default();
    Some(TsConfigFile {
        path: path.to_path_buf(),
        dir,
        extends: own.extends.clone(),
        ancestors,
        options,
        include: patterns(&merged.include),
        exclude: patterns(&merged.exclude),
        files: patterns(&merged.files).map(|f| f.into_iter().map(PathBuf::from).collect()),
        references,
        is_program: false,
        root_files: Vec::new(),
        doc: own.doc,
    })
}

fn reference(
    doc: &JsoncDoc,
    dir: &Path,
    index: usize,
    node: &oxiclean_core::jsonc_tree::Node,
) -> Option<Reference> {
    let raw = node.get("path").and_then(|p| doc.value(p))?.as_str()?.to_string();
    let target = path_clean::clean(dir.join(&raw));
    let target = if target.extension().is_some_and(|e| e == "json") {
        target
    } else {
        target.join("tsconfig.json")
    };
    Some(Reference {
        raw,
        target,
        line: doc.line(doc.root.get("references")?.items()[index].span.start),
    })
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
    if depth > MAX_EXTENDS_DEPTH || !seen.insert(path.to_path_buf()) {
        return;
    }
    let Some(Some(r)) = raw.get(path) else { return };
    for e in &r.extends {
        merge(e, raw, out, seen, depth + 1);
    }
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
        let mut out: BTreeSet<PathBuf> = BTreeSet::new();
        let include = match (&config.include, &config.files) {
            (Some(i), _) => i.clone(),
            (None, Some(_)) => Vec::new(),
            (None, None) => vec![format!("{}/**/*", slashes(&config.dir))],
        };
        let exclude = config.exclude.clone().unwrap_or_else(|| {
            let mut d: Vec<String> =
                SKIPPED_DIRS.iter().map(|s| format!("{}/{s}", slashes(&config.dir))).collect();
            d.extend(config.path_option("outDir").map(|o| slashes(&o)));
            d
        });
        let exclude_set = globs(&exclude, true);
        let mut wildcard: BTreeSet<PathBuf> = BTreeSet::new();
        for pattern in &include {
            let pattern = as_files_pattern(pattern);
            let Some(set) = globs(std::slice::from_ref(&pattern), false) else { continue };
            let base = literal_base(&pattern);
            // Wildcards skip hidden files unless the pattern names them.
            let explicit_hidden =
                pattern.strip_prefix(&slashes(&base)).unwrap_or(&pattern).contains("/.");
            for f in self.under(&base) {
                let rel = f.strip_prefix(&base).unwrap_or(f);
                let hidden =
                    rel.components().any(|c| c.as_os_str().to_string_lossy().starts_with('.'));
                if (hidden && !explicit_hidden) || !supported(f) {
                    continue;
                }
                let text = slashes(f);
                if set.is_match(&text) && !exclude_set.as_ref().is_some_and(|e| e.is_match(&text)) {
                    wildcard.insert(f.clone());
                }
            }
        }
        // A `.d.ts` or `.js` next to a `.ts` of the same name is its
        // output, and `tsc` drops it from wildcard matches.
        let stems: BTreeSet<String> = wildcard
            .iter()
            .filter(|f| matches!(extension_of(f), Some(".ts" | ".tsx")))
            .map(|f| stem(f))
            .collect();
        out.extend(wildcard.into_iter().filter(|f| {
            !matches!(extension_of(f), Some(".d.ts" | ".js" | ".jsx")) || !stems.contains(&stem(f))
        }));
        if let Some(files) = &config.files {
            out.extend(files.iter().filter(|f| f.is_file()).cloned());
        }
        out.into_iter().collect()
    }
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
        let escaped: String = p
            .chars()
            .flat_map(|c| match c {
                '[' | ']' | '{' | '}' => vec!['[', c, ']'],
                c => vec![c],
            })
            .collect();
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

/// The directories of `pattern` before its first wildcard.
fn literal_base(pattern: &str) -> PathBuf {
    let mut base = PathBuf::new();
    for part in pattern.split('/') {
        if part.contains(['*', '?']) {
            break;
        }
        base.push(if part.is_empty() { "/" } else { part });
    }
    // A pattern naming one file has its directory as the base.
    if !pattern.contains(['*', '?']) {
        base.pop();
    }
    base
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
    fn relative_paths() {
        assert_eq!(relative(Path::new("/a/b"), Path::new("/a/b/dist")), "dist");
        assert_eq!(relative(Path::new("/a/b"), Path::new("/a/c")), "../c");
        assert_eq!(relative(Path::new("/a/b"), Path::new("/a/b")), ".");
    }
}
