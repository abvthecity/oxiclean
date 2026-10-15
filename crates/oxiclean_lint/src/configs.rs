//! Finding a project's ESLint and oxlint configs and reading each into one
//! model: an ordered list of [`Entry`]s, each applying to some files, and
//! the patterns the config ignores.
//!
//! - ESLint 8 (`.eslintrc.*`, package.json `eslintConfig`): the config
//!   itself, then each of `overrides`. Configs it `extends` by path, or
//!   from a workspace package, are read in place.
//! - ESLint 9 flat configs (`eslint.config.*`): each element of the
//!   exported array, through `defineConfig`, `tseslint.config`, spreads,
//!   `extends` and constants. Conditional elements are kept but marked;
//!   elements that can't be read, such as a shared config from another
//!   package, are opaque.
//! - oxlint (`.oxlintrc.json`, `oxlint.json`, `oxlint.config.*`).

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use ignore::WalkBuilder;
use oxiclean_core::Project;
use tracing::debug;

use crate::js::{self, Item, Resolver, SPREAD, Val};

/// Flat config file names, in ESLint's order.
const FLAT_FILES: &[&str] = &[
    "eslint.config.js",
    "eslint.config.mjs",
    "eslint.config.cjs",
    "eslint.config.ts",
    "eslint.config.mts",
    "eslint.config.cts",
];

/// eslintrc file names, in ESLint 8's order. YAML ones aren't read.
const ESLINTRC_FILES: &[&str] = &[".eslintrc.js", ".eslintrc.cjs", ".eslintrc.json", ".eslintrc"];

/// oxlint config file names.
const OXLINT_FILES: &[&str] = &[
    ".oxlintrc.json",
    ".oxlintrc.jsonc",
    "oxlint.json",
    "oxlint.config.ts",
    "oxlint.config.mts",
    "oxlint.config.cts",
    "oxlint.config.js",
    "oxlint.config.mjs",
    "oxlint.config.cjs",
];

/// Directories of test inputs: the configs in them are data for a
/// tool's tests, not configs anyone lints with.
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

/// Extensions of the files linters read as JavaScript.
pub(crate) const JS_EXTENSIONS: &[&str] = &["js", "mjs", "cjs", "jsx"];

/// Which linter reads a config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Tool {
    /// ESLint 8's `.eslintrc` format.
    Eslintrc,
    /// ESLint 9's flat config.
    Flat,
    Oxlint,
}

/// The files an entry applies to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Files {
    All,
    Some(Vec<String>),
    /// `files` is set to something that can't be read.
    Unknown,
}

/// Whether an entry turns type-aware parsing on or off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Typed {
    On,
    Off,
    Unknown,
}

/// `parserOptions.project` as set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProjectOption {
    /// `project: true`: the nearest tsconfig of each file.
    Nearest,
    /// Paths or globs of tsconfigs.
    Paths(Vec<String>),
}

/// A rule an entry sets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuleUse {
    pub name: String,
    pub on: bool,
    pub line: u32,
}

/// One part of a config that applies to some files.
#[derive(Debug, Clone)]
pub(crate) struct Entry {
    pub files: Files,
    /// Patterns of files the entry doesn't apply to.
    pub excludes: Vec<String>,
    /// Whether it applies only under a condition, such as an environment
    /// variable.
    pub conditional: bool,
    /// Whether it couldn't be read, so it may set anything.
    pub opaque: bool,
    /// Type-aware parsing, and the line that sets it.
    pub typed: Option<(Typed, u32)>,
    /// `parserOptions.project`, and its line.
    pub project: Option<(ProjectOption, u32)>,
    /// Whether it sets `parserOptions.tsconfigRootDir`.
    pub root_dir: bool,
    /// The parser it sets: typescript-eslint's (`Some(true)`) or another.
    pub ts_parser: Option<bool>,
    pub rules: Vec<RuleUse>,
    pub line: u32,
}

impl Entry {
    fn new(files: Files, line: u32, conditional: bool) -> Self {
        Self {
            files,
            excludes: Vec::new(),
            conditional,
            opaque: false,
            typed: None,
            project: None,
            root_dir: false,
            ts_parser: None,
            rules: Vec::new(),
            line,
        }
    }
}

/// Whether a flat config parser value is typescript-eslint's parser.
fn is_ts_parser(v: &Val) -> Option<bool> {
    match v {
        Val::Import { source, path } => Some(
            (source == "typescript-eslint" && path == "parser")
                || (source == "@typescript-eslint/parser" && !path.starts_with("configs")),
        ),
        Val::Obj(_) | Val::Str(_) => Some(false),
        _ => None,
    }
}

/// Whether a reference to typescript-eslint's configs is one that sets its
/// parser for every file: all but `eslintRecommended` and
/// `disableTypeChecked`, which build on `base`.
fn sets_ts_parser(source: &str, path: &str) -> bool {
    let from_ts = matches!(source, "typescript-eslint" | "@typescript-eslint/eslint-plugin");
    from_ts
        && path.starts_with("configs")
        && !path.contains("eslintRecommended")
        && !path.contains("eslint-recommended")
        && !disables_types(path)
}

/// Whether eslintrc `extends` name sets typescript-eslint's parser.
fn eslintrc_sets_ts_parser(name: &str) -> bool {
    name.strip_prefix("plugin:@typescript-eslint/")
        .is_some_and(|c| c != "eslint-recommended" && c != "disable-type-checked")
}

/// One lint config, read.
#[derive(Debug, Clone)]
pub(crate) struct LintConfig {
    pub tool: Tool,
    pub path: PathBuf,
    pub dir: PathBuf,
    /// Where its JSON keeps its settings, for fixes: the root, or
    /// package.json's `eslintConfig`. `None` when it isn't JSON.
    pub json_prefix: Option<Vec<&'static str>>,
    pub entries: Vec<Entry>,
    /// Patterns of files it ignores, relative to its directory.
    pub ignores: Vec<String>,
    /// Line of its ignores, or of the config.
    pub ignores_line: u32,
    /// Whether it may ignore more than `ignores` says.
    pub ignores_unknown: bool,
    /// Whether it ignores what `.gitignore` files ignore.
    pub gitignore: bool,
    /// Whether it uses eslint-plugin-oxlint.
    pub oxlint_plugin: bool,
    /// eslintrc's `root: true`.
    pub root: bool,
    /// oxlint's `options.typeAware: true`, and its line.
    pub type_aware: Option<u32>,
    /// oxlint `categories`, as set.
    pub categories: Vec<(String, bool)>,
    /// oxlint `plugins`, when set.
    pub plugins: Option<Vec<String>>,
    /// The line of the config's start.
    pub line: u32,
}

impl LintConfig {
    fn new(tool: Tool, path: PathBuf, json_prefix: Option<Vec<&'static str>>) -> Self {
        let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        Self {
            tool,
            path,
            dir,
            json_prefix,
            entries: Vec::new(),
            ignores: Vec::new(),
            ignores_line: 0,
            ignores_unknown: false,
            gitignore: false,
            oxlint_plugin: false,
            root: false,
            type_aware: None,
            categories: Vec::new(),
            plugins: None,
            line: 1,
        }
    }

    /// Whether some part of it couldn't be read.
    pub(crate) fn opaque(&self) -> bool {
        self.entries.iter().any(|e| e.opaque)
    }
}

/// What a walk of the project finds.
#[derive(Debug, Default)]
pub(crate) struct Scan {
    /// Config files, with the linter that reads each.
    pub configs: Vec<(Tool, PathBuf)>,
    /// Directories with a package.json, deno.json or deno.jsonc.
    pub package_dirs: Vec<PathBuf>,
    /// JavaScript files git doesn't ignore.
    pub js_files: Vec<PathBuf>,
}

/// Walks the project under `root`, skipping what `.gitignore` files
/// ignore and `node_modules`.
pub(crate) fn scan(root: &Path) -> Scan {
    let mut out = Scan::default();
    let walker = WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .require_git(false)
        .parents(false)
        .filter_entry(|e| e.file_name() != "node_modules" && e.file_name() != ".git")
        .build();
    for entry in walker.filter_map(Result::ok) {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.into_path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        let in_test_inputs = || {
            path.strip_prefix(root).is_ok_and(|rel| {
                rel.components().any(|c| TEST_INPUT_DIRS.iter().any(|d| c.as_os_str() == *d))
            })
        };
        if matches!(name, "package.json" | "deno.json" | "deno.jsonc") {
            if let Some(dir) = path.parent() {
                out.package_dirs.push(dir.to_path_buf());
            }
            if name == "package.json" && !in_test_inputs() {
                out.configs.push((Tool::Eslintrc, path.clone()));
            }
        }
        let tool = if FLAT_FILES.contains(&name) {
            Some(Tool::Flat)
        } else if ESLINTRC_FILES.contains(&name) {
            Some(Tool::Eslintrc)
        } else if OXLINT_FILES.contains(&name) {
            Some(Tool::Oxlint)
        } else {
            None
        };
        if let Some(tool) = tool
            && !in_test_inputs()
        {
            out.configs.push((tool, path.clone()));
        }
        if path.extension().and_then(|e| e.to_str()).is_some_and(|e| JS_EXTENSIONS.contains(&e)) {
            out.js_files.push(path);
        }
    }
    out.package_dirs.sort();
    out.package_dirs.dedup();
    out.configs.sort();
    out.js_files.sort();
    out
}

/// Reads the configs `scan` found. package.json files without
/// `eslintConfig`, and configs that can't be read, are dropped.
pub(crate) fn read_all(project: &Project, scan: &Scan) -> Vec<LintConfig> {
    let resolver = Resolver { project };
    let mut out = Vec::new();
    for (tool, path) in &scan.configs {
        let config = match tool {
            Tool::Flat => Some(read_flat(path, &resolver)),
            Tool::Eslintrc => read_eslintrc(path, &resolver),
            Tool::Oxlint => read_oxlint(path, &resolver),
        };
        match config {
            Some(c) => out.push(c),
            // Most package.json files have no `eslintConfig`.
            None if path.ends_with("package.json") => {}
            None => debug!("Couldn't read {}", path.display()),
        }
    }
    out
}

/// The severity `v` sets a rule to: on, off, or unknown.
fn severity(v: &Val) -> Option<bool> {
    match v {
        Val::Str(s) => match s.as_str() {
            "off" | "allow" => Some(false),
            "warn" | "error" | "deny" => Some(true),
            _ => None,
        },
        Val::Num(n) => Some(*n != 0.0),
        Val::Arr(items) => items.first().filter(|i| !i.spread).and_then(|i| severity(&i.val)),
        _ => None,
    }
}

/// The rules `rules` sets.
fn rules(rules: Option<&Val>) -> Vec<RuleUse> {
    let Some(rules) = rules else { return Vec::new() };
    rules
        .props()
        .iter()
        .filter(|p| p.key != SPREAD)
        .filter_map(|p| Some(RuleUse { name: p.key.clone(), on: severity(&p.val)?, line: p.line }))
        .collect()
}

/// Reads `parserOptions` into `entry`.
fn parser_options(entry: &mut Entry, options: Option<&Val>) {
    let Some(options) = options else { return };
    let project = options.prop("project");
    let service = options.prop("projectService");
    let on = |v: &Val| match v.truthy() {
        Some(true) => Typed::On,
        Some(false) => Typed::Off,
        None => Typed::Unknown,
    };
    let typed = match (project.map(|p| (on(&p.val), p.line)), service.map(|p| (on(&p.val), p.line)))
    {
        (Some((Typed::On, l)), _) | (_, Some((Typed::On, l))) => Some((Typed::On, l)),
        (Some((Typed::Unknown, l)), _) | (_, Some((Typed::Unknown, l))) => {
            Some((Typed::Unknown, l))
        }
        (Some((Typed::Off, l)), _) | (_, Some((Typed::Off, l))) => Some((Typed::Off, l)),
        (None, None) => None,
    };
    if typed.is_some() {
        entry.typed = typed;
    }
    if let Some(p) = project {
        entry.project = match &p.val {
            Val::Bool(true) => Some((ProjectOption::Nearest, p.line)),
            v => v.strings().map(|s| (ProjectOption::Paths(s), p.line)),
        };
    }
    if options.get("tsconfigRootDir").is_some() {
        entry.root_dir = true;
    }
    if options.get(SPREAD).is_some() {
        entry.opaque = true;
    }
}

/// Whether a package's configs can be trusted to set no ignores, and
/// nothing that turns type-aware parsing off for other files: ESLint's
/// own packages, typescript-eslint, and plugins.
fn known_package(source: &str) -> bool {
    matches!(
        source,
        "@eslint/js"
            | "typescript-eslint"
            | "globals"
            | "eslint/config"
            | "eslint-config-prettier"
            | "eslint-config-prettier/flat"
    ) || source.starts_with("@eslint/")
        || source.starts_with("@typescript-eslint/")
        || source.contains("eslint-plugin")
}

/// Whether a reference to what a package exports turns type-aware
/// parsing off, as typescript-eslint's `disableTypeChecked`.
fn disables_types(path: &str) -> bool {
    path.split('.').any(|p| p == "disableTypeChecked")
}

fn read_flat(path: &Path, resolver: &Resolver<'_>) -> LintConfig {
    let loaded = js::load(path, resolver);
    let mut c = LintConfig::new(Tool::Flat, path.to_path_buf(), None);
    c.oxlint_plugin = loaded.packages.iter().any(|p| p.contains("eslint-plugin-oxlint"));
    match &loaded.value {
        Some(v) => {
            let mut b = FlatReader { config: &mut c };
            b.element(v, &Files::All, false, 1);
        }
        None => {
            let mut e = Entry::new(Files::All, 1, false);
            e.opaque = true;
            c.entries.push(e);
        }
    }
    if c.opaque() {
        c.ignores_unknown = true;
    }
    c
}

struct FlatReader<'c> {
    config: &'c mut LintConfig,
}

impl FlatReader<'_> {
    fn opaque(&mut self, files: &Files, conditional: bool, line: u32) {
        let mut e = Entry::new(files.clone(), line, conditional);
        e.opaque = true;
        self.config.entries.push(e);
    }

    fn elements(&mut self, items: &[Item], files: &Files, conditional: bool) {
        for i in items {
            self.element(&i.val, files, conditional, i.line);
        }
    }

    /// Reads one element of the config array, or what a spread or call
    /// makes of several.
    fn element(&mut self, v: &Val, files: &Files, conditional: bool, line: u32) {
        match v {
            Val::Arr(items) => self.elements(items, files, conditional),
            Val::Either(a, b) => {
                self.element(a, files, true, line);
                self.element(b, files, true, line);
            }
            // The other side of `cond && config`.
            Val::Null | Val::Bool(false) => {}
            Val::Obj(_) => self.object(v, files, conditional, line),
            Val::Call { callee, source, args } => {
                self.call(callee, source.as_deref(), args, files, conditional, line);
            }
            Val::Import { source, path } => {
                if disables_types(path) {
                    let mut e = Entry::new(files.clone(), line, conditional);
                    e.typed = Some((Typed::Off, line));
                    self.config.entries.push(e);
                } else if sets_ts_parser(source, path) {
                    let mut e = Entry::new(files.clone(), line, conditional);
                    e.ts_parser = Some(true);
                    self.config.entries.push(e);
                } else if source.contains("eslint-plugin-oxlint") {
                    self.config.oxlint_plugin = true;
                } else if !known_package(source) {
                    self.opaque(files, conditional, line);
                }
            }
            _ => self.opaque(files, conditional, line),
        }
    }

    fn call(
        &mut self,
        callee: &str,
        source: Option<&str>,
        args: &[Item],
        files: &Files,
        conditional: bool,
        line: u32,
    ) {
        let last = callee.rsplit('.').next().unwrap_or_default();
        let from_package = source.is_some_and(|s| !s.starts_with('.'));
        if source.is_some_and(|s| s.contains("eslint-plugin-oxlint")) {
            self.config.oxlint_plugin = true;
            return;
        }
        match last {
            "defineConfig" | "defineFlatConfig" | "fixupConfigRules" => {
                self.elements(args, files, conditional);
            }
            "config" if from_package || callee == "config" => {
                self.elements(args, files, conditional);
            }
            "globalIgnores" => match args.first().and_then(|a| a.val.strings()) {
                Some(patterns) => {
                    self.config.ignores.extend(patterns);
                    self.config.ignores_line = line;
                }
                None => self.config.ignores_unknown = true,
            },
            "includeIgnoreFile" => self.config.gitignore = true,
            _ if source.is_some_and(|s| s.contains("gitignore")) => self.config.gitignore = true,
            // FlatCompat's `compat.extends('plugin:x/y')` and
            // `compat.config({...})`, on a local `new FlatCompat()`.
            "extends" | "config" if !from_package => {
                let names: Vec<String> =
                    args.iter().filter_map(|a| a.val.as_str()).map(str::to_string).collect();
                if last == "config" {
                    if let Some(obj) = args.first().filter(|a| matches!(a.val, Val::Obj(_))) {
                        let mut entries = Vec::new();
                        let mut ignores = Vec::new();
                        let mut c = Compat { oxlint_plugin: false, ignores_unknown: false };
                        eslintrc_entries(
                            &obj.val,
                            files,
                            conditional,
                            &mut entries,
                            &mut ignores,
                            &mut c,
                        );
                        self.config.entries.extend(entries);
                        self.config.ignores.extend(ignores);
                        self.config.oxlint_plugin |= c.oxlint_plugin;
                        self.config.ignores_unknown |= c.ignores_unknown;
                    } else {
                        self.opaque(files, conditional, line);
                    }
                    return;
                }
                if names.len() != args.len() {
                    self.opaque(files, conditional, line);
                    return;
                }
                for n in &names {
                    self.extends_name(n, files, conditional, line);
                }
            }
            _ if source.is_some_and(known_package) => {}
            _ => self.opaque(files, conditional, line),
        }
    }

    /// An eslintrc `extends` name, through FlatCompat.
    fn extends_name(&mut self, name: &str, files: &Files, conditional: bool, line: u32) {
        if name.starts_with("plugin:oxlint/") {
            self.config.oxlint_plugin = true;
        } else if name == "plugin:@typescript-eslint/disable-type-checked" {
            let mut e = Entry::new(files.clone(), line, conditional);
            e.typed = Some((Typed::Off, line));
            self.config.entries.push(e);
        } else if eslintrc_sets_ts_parser(name) {
            let mut e = Entry::new(files.clone(), line, conditional);
            e.ts_parser = Some(true);
            self.config.entries.push(e);
        } else if !name.starts_with("plugin:") && !name.starts_with("eslint:") {
            // A shareable config may set anything.
            self.opaque(files, conditional, line);
        }
    }

    fn object(&mut self, v: &Val, files: &Files, conditional: bool, line: u32) {
        let keys: BTreeSet<&str> = v.props().iter().map(|p| p.key.as_str()).collect();
        if keys.contains("ignores") && keys.iter().all(|k| matches!(*k, "ignores" | "name")) {
            match v.get("ignores").and_then(Val::strings) {
                Some(patterns) => {
                    self.config.ignores.extend(patterns);
                    self.config.ignores_line = v.prop("ignores").map_or(line, |p| p.line);
                }
                None => self.config.ignores_unknown = true,
            }
            return;
        }
        if keys.contains("basePath") {
            self.config.ignores_unknown = true;
        }
        let own_files = match v.get("files") {
            None => files.clone(),
            Some(f) => match flatten_strings(f) {
                Some(patterns) => Files::Some(patterns),
                None => Files::Unknown,
            },
        };
        // `extends` come first, with the object's files.
        if let Some(Val::Arr(items)) = v.get("extends") {
            self.elements(items, &own_files, conditional);
        } else if v.get("extends").is_some() {
            self.opaque(&own_files, conditional, line);
        }
        let mut e = Entry::new(own_files, line, conditional);
        e.excludes = v.get("ignores").and_then(Val::strings).unwrap_or_default();
        let language = v.get("languageOptions");
        parser_options(&mut e, language.and_then(|l| l.get("parserOptions")));
        if let Some(parser) = language.and_then(|l| l.get("parser")) {
            e.ts_parser = is_ts_parser(parser);
            if e.ts_parser.is_none() {
                e.opaque = true;
            }
        }
        e.rules = rules(v.get("rules"));
        for p in v.props().iter().filter(|p| p.key == SPREAD) {
            match &p.val {
                Val::Import { path, .. } if disables_types(path) => {
                    e.typed = Some((Typed::Off, p.line));
                }
                Val::Import { source, path } if sets_ts_parser(source, path) => {
                    e.ts_parser = Some(true);
                }
                Val::Import { source, .. } if known_package(source) => {}
                Val::Import { source, .. } if source.contains("eslint-plugin-oxlint") => {
                    self.config.oxlint_plugin = true;
                }
                _ => e.opaque = true,
            }
        }
        if v.get("languageOptions").is_some_and(|l| l.get(SPREAD).is_some()) {
            e.opaque = true;
        }
        self.config.entries.push(e);
    }
}

/// Strings of `files`, where flat configs may nest arrays (all of whose
/// patterns must match); a nested array is read as its first pattern.
fn flatten_strings(v: &Val) -> Option<Vec<String>> {
    match v {
        Val::Str(s) => Some(vec![s.clone()]),
        Val::Arr(items) => {
            let mut out = Vec::new();
            for i in items {
                match &i.val {
                    Val::Str(s) if !i.spread => out.push(s.clone()),
                    Val::Arr(inner) if !i.spread => {
                        out.push(inner.first()?.val.as_str()?.to_string());
                    }
                    v @ Val::Arr(_) if i.spread => out.extend(flatten_strings(v)?),
                    _ => return None,
                }
            }
            Some(out)
        }
        _ => None,
    }
}

/// What reading an eslintrc object found besides its entries.
struct Compat {
    oxlint_plugin: bool,
    ignores_unknown: bool,
}

/// The entries of eslintrc object `v`: itself, then each of `overrides`,
/// whose files are relative to the config. Configs it extends by path
/// aren't read here.
fn eslintrc_entries(
    v: &Val,
    files: &Files,
    conditional: bool,
    entries: &mut Vec<Entry>,
    ignores: &mut Vec<String>,
    c: &mut Compat,
) {
    let line = 1;
    for name in v.get("extends").and_then(Val::strings).unwrap_or_default() {
        if name.starts_with("plugin:oxlint/") {
            c.oxlint_plugin = true;
        }
        if name == "plugin:@typescript-eslint/disable-type-checked" {
            let mut e = Entry::new(files.clone(), line, conditional);
            e.typed = Some((Typed::Off, line));
            entries.push(e);
        } else if eslintrc_sets_ts_parser(&name) {
            let mut e = Entry::new(files.clone(), line, conditional);
            e.ts_parser = Some(true);
            entries.push(e);
        }
    }
    if names_plugin(v.get("plugins"), "oxlint") {
        c.oxlint_plugin = true;
    }
    let mut e = Entry::new(files.clone(), v.props().first().map_or(1, |p| p.line), conditional);
    parser_options(&mut e, v.get("parserOptions"));
    match v.get("parser") {
        Some(Val::Str(p)) => e.ts_parser = Some(p.contains("@typescript-eslint/parser")),
        // `require.resolve('@typescript-eslint/parser')`.
        Some(Val::Call { callee, args, .. }) if callee == "require.resolve" => {
            e.ts_parser = args
                .first()
                .and_then(|a| a.val.as_str())
                .map(|p| p.contains("@typescript-eslint/parser"));
            if e.ts_parser.is_none() {
                e.opaque = true;
            }
        }
        Some(_) => e.opaque = true,
        None => {}
    }
    e.rules = rules(v.get("rules"));
    entries.push(e);
    if let Some(p) = v.get("ignorePatterns") {
        match p.strings() {
            Some(patterns) => ignores.extend(patterns),
            None => c.ignores_unknown = true,
        }
    }
    if let Some(Val::Arr(overrides)) = v.get("overrides") {
        for o in overrides {
            let files = match o.val.get("files").map(Val::strings) {
                Some(Some(p)) => Files::Some(p),
                _ => Files::Unknown,
            };
            let mut inner = Vec::new();
            eslintrc_entries(&o.val, &files, conditional, &mut inner, &mut Vec::new(), c);
            let excludes = o.val.get("excludedFiles").and_then(Val::strings).unwrap_or_default();
            for mut e in inner {
                e.line = o.line.max(e.line);
                e.excludes.extend(excludes.iter().cloned());
                entries.push(e);
            }
        }
    }
}

/// Whether eslintrc `plugins` names `name`.
fn names_plugin(plugins: Option<&Val>, name: &str) -> bool {
    plugins
        .and_then(Val::strings)
        .is_some_and(|p| p.iter().any(|p| p == name || p == &format!("eslint-plugin-{name}")))
}

/// The package an eslintrc `extends` name loads, as ESLint resolves it.
fn shareable_package(name: &str) -> Option<String> {
    if name.starts_with("plugin:") || name.starts_with("eslint:") {
        return None;
    }
    let (pkg, sub) = oxiclean_core::split_package_request(name);
    let pkg = if let Some(scope) = pkg.strip_prefix('@') {
        match scope.split_once('/') {
            None => format!("@{scope}/eslint-config"),
            Some((s, n)) if n.starts_with("eslint-config") => format!("@{s}/{n}"),
            Some((s, n)) => format!("@{s}/eslint-config-{n}"),
        }
    } else if pkg.starts_with("eslint-config") {
        pkg.to_string()
    } else {
        format!("eslint-config-{pkg}")
    };
    Some(if sub.is_empty() { pkg } else { format!("{pkg}/{sub}") })
}

/// Reads an eslintrc config, and the configs it extends in the project.
fn read_eslintrc(path: &Path, resolver: &Resolver<'_>) -> Option<LintConfig> {
    let is_package = path.file_name().is_some_and(|n| n == "package.json");
    let loaded = js::load(path, resolver);
    let value = loaded.value?;
    let (value, prefix) = if is_package {
        (value.get("eslintConfig")?.clone(), vec!["eslintConfig"])
    } else {
        (value, Vec::new())
    };
    if !matches!(value, Val::Obj(_)) {
        return None;
    }
    let json = path.extension().is_none_or(|e| e == "json");
    let mut c = LintConfig::new(Tool::Eslintrc, path.to_path_buf(), json.then_some(prefix));
    c.line = value.props().first().map_or(1, |p| p.line);
    c.root = value.get("root") == Some(&Val::Bool(true));
    c.ignores_line = value.prop("ignorePatterns").map_or(c.line, |p| p.line);
    c.oxlint_plugin = loaded.packages.iter().any(|p| p.contains("eslint-plugin-oxlint"));
    let mut compat = Compat { oxlint_plugin: false, ignores_unknown: false };
    let mut entries = Vec::new();
    let mut ignores = Vec::new();
    extended(&value, &c.dir, resolver, &mut entries, &mut ignores, &mut compat, 0);
    eslintrc_entries(&value, &Files::All, false, &mut entries, &mut ignores, &mut compat);
    c.entries = entries;
    c.ignores = ignores;
    c.oxlint_plugin |= compat.oxlint_plugin;
    c.ignores_unknown = compat.ignores_unknown;
    Some(c)
}

/// Reads the configs eslintrc object `v` extends by path or from a
/// workspace package, first, as ESLint merges them under it. Their
/// entries keep the line of `extends` in the config being read.
fn extended(
    v: &Val,
    dir: &Path,
    resolver: &Resolver<'_>,
    entries: &mut Vec<Entry>,
    ignores: &mut Vec<String>,
    c: &mut Compat,
    depth: usize,
) {
    let Some(line) = v.prop("extends").map(|p| p.line) else { return };
    for name in v.get("extends").and_then(Val::strings).unwrap_or_default() {
        let file = if name.starts_with('.') || name.starts_with('/') {
            let f = js::resolve_path(&dir.join(&name));
            if f.is_none() {
                c.ignores_unknown = true;
            }
            f
        } else {
            shareable_package(&name).and_then(|p| resolver.resolve(dir, &p))
        };
        let Some(file) = file else { continue };
        if depth > 4 {
            c.ignores_unknown = true;
            continue;
        }
        let loaded = js::load(&file, resolver);
        if loaded.packages.iter().any(|p| p.contains("eslint-plugin-oxlint")) {
            c.oxlint_plugin = true;
        }
        let Some(base) = loaded.value else {
            c.ignores_unknown = true;
            continue;
        };
        let base_dir = file.parent().map(Path::to_path_buf).unwrap_or_default();
        let mut inner = Vec::new();
        extended(&base, &base_dir, resolver, &mut inner, ignores, c, depth + 1);
        eslintrc_entries(&base, &Files::All, false, &mut inner, ignores, c);
        for mut e in inner {
            e.line = line;
            for r in &mut e.rules {
                r.line = line;
            }
            if let Some((t, _)) = e.typed {
                e.typed = Some((t, line));
            }
            if let Some((p, _)) = e.project.take() {
                e.project = Some((p, line));
            }
            entries.push(e);
        }
    }
}

/// Reads an oxlint config, with the ignores and rules of the configs it
/// extends.
fn read_oxlint(path: &Path, resolver: &Resolver<'_>) -> Option<LintConfig> {
    let value = js::load(path, resolver).value?;
    if !matches!(value, Val::Obj(_)) {
        return None;
    }
    let json = path.extension().is_some_and(|e| e == "json" || e == "jsonc");
    let mut c = LintConfig::new(Tool::Oxlint, path.to_path_buf(), json.then(Vec::new));
    c.line = value.props().first().map_or(1, |p| p.line);
    c.ignores_line = value.prop("ignorePatterns").map_or(c.line, |p| p.line);
    read_oxlint_value(&mut c, &value, resolver, None, 0);
    Some(c)
}

fn read_oxlint_value(
    c: &mut LintConfig,
    v: &Val,
    resolver: &Resolver<'_>,
    line: Option<u32>,
    depth: usize,
) {
    let dir = c.dir.clone();
    if let Some(ext) = v.prop("extends") {
        for name in ext.val.strings().unwrap_or_default() {
            match js::resolve_path(&dir.join(&name)) {
                Some(f) if depth < 4 => match js::load(&f, resolver).value {
                    Some(base) => {
                        read_oxlint_value(
                            c,
                            &base,
                            resolver,
                            Some(line.unwrap_or(ext.line)),
                            depth + 1,
                        );
                    }
                    None => c.ignores_unknown = true,
                },
                _ => c.ignores_unknown = true,
            }
        }
    }
    match v.get("ignorePatterns").map(Val::strings) {
        Some(Some(p)) => c.ignores.extend(p),
        Some(None) => c.ignores_unknown = true,
        None => {}
    }
    let mut e = Entry::new(Files::All, line.unwrap_or(c.line), false);
    e.rules = rules(v.get("rules"));
    if let Some(l) = line {
        for r in &mut e.rules {
            r.line = l;
        }
    }
    c.entries.push(e);
    if let Some(Val::Arr(overrides)) = v.get("overrides") {
        for o in overrides {
            let files = match o.val.get("files").map(Val::strings) {
                Some(Some(p)) => Files::Some(p),
                _ => Files::Unknown,
            };
            let mut e = Entry::new(files, line.unwrap_or(o.line), false);
            e.rules = rules(o.val.get("rules"));
            if let Some(l) = line {
                for r in &mut e.rules {
                    r.line = l;
                }
            }
            c.entries.push(e);
        }
    }
    for p in v.get("categories").map(Val::props).unwrap_or_default() {
        if let Some(on) = severity(&p.val) {
            c.categories.push((p.key.clone(), on));
        }
    }
    if let Some(plugins) = v.get("plugins").and_then(Val::strings) {
        c.plugins = Some(plugins);
    }
    if let Some(p) = v.at(&["options", "typeAware"])
        && p.val == Val::Bool(true)
        && line.is_none()
    {
        c.type_aware = Some(p.line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn config(name: &str, src: &str) -> LintConfig {
        let t = tempfile::TempDir::new().unwrap();
        let path = t.path().join(name);
        fs::write(&path, src).unwrap();
        let project = Project::new(t.path());
        let resolver = Resolver { project: &project };
        match name {
            n if n.starts_with("eslint.config") => read_flat(&path, &resolver),
            n if n.contains("oxlint") => read_oxlint(&path, &resolver).unwrap(),
            _ => read_eslintrc(&path, &resolver).unwrap(),
        }
    }

    #[test]
    fn reads_shareable_package_names() {
        assert_eq!(shareable_package("airbnb").as_deref(), Some("eslint-config-airbnb"));
        assert_eq!(shareable_package("@acme").as_deref(), Some("@acme/eslint-config"));
        assert_eq!(shareable_package("@acme/web").as_deref(), Some("@acme/eslint-config-web"));
        assert_eq!(shareable_package("plugin:x/y"), None);
    }

    #[test]
    fn reads_flat_entries() {
        let c = config(
            "eslint.config.js",
            "import tseslint from 'typescript-eslint';\nimport shared from '@acme/lint';\nexport default tseslint.config(\n  { ignores: ['dist/**'] },\n  { files: ['**/*.ts'], languageOptions: { parserOptions: { projectService: true } } },\n  { files: ['**/*.js'], ...tseslint.configs.disableTypeChecked },\n  { rules: { 'import/no-cycle': ['error', { maxDepth: 3 }], eqeqeq: 'off' } },\n  ...(process.env.CI ? [shared] : []),\n);\n",
        );
        assert_eq!(c.ignores, ["dist/**"]);
        assert_eq!(c.ignores_line, 4);
        let e = &c.entries;
        assert_eq!(e[0].files, Files::Some(vec!["**/*.ts".into()]));
        assert_eq!(e[0].typed, Some((Typed::On, 5)));
        assert_eq!(e[1].typed, Some((Typed::Off, 6)));
        assert_eq!(
            e[2].rules,
            [
                RuleUse { name: "import/no-cycle".into(), on: true, line: 7 },
                RuleUse { name: "eqeqeq".into(), on: false, line: 7 },
            ]
        );
        assert!(e[3].opaque && e[3].conditional);
        assert!(c.ignores_unknown);
    }

    #[test]
    fn reads_eslintrc_overrides() {
        let c = config(
            ".eslintrc.json",
            "{\n  \"parserOptions\": { \"project\": \"./tsconfig.json\" },\n  \"ignorePatterns\": \"dist\",\n  \"overrides\": [\n    { \"files\": \"*.js\", \"extends\": [\"plugin:@typescript-eslint/disable-type-checked\"] }\n  ]\n}\n",
        );
        assert_eq!(c.ignores, ["dist"]);
        assert_eq!(
            c.entries[0].project,
            Some((ProjectOption::Paths(vec!["./tsconfig.json".into()]), 2))
        );
        assert_eq!(c.entries[1].files, Files::Some(vec!["*.js".into()]));
        assert_eq!(c.entries[1].typed.map(|t| t.0), Some(Typed::Off));
        assert_eq!(c.json_prefix, Some(vec![]));
    }

    #[test]
    fn reads_oxlint() {
        let c = config(
            ".oxlintrc.json",
            "{\n  \"categories\": { \"correctness\": \"off\" },\n  \"options\": { \"typeAware\": true },\n  \"rules\": { \"eqeqeq\": \"warn\" }\n}\n",
        );
        assert_eq!(c.type_aware, Some(3));
        assert_eq!(c.categories, [("correctness".to_string(), false)]);
        assert_eq!(c.entries[0].rules[0].name, "eqeqeq");
    }
}
