//! Finding a project's ESLint and oxlint configs and reading each into one
//! model: an ordered list of [`Entry`]s, each applying to some files, and
//! the patterns the config ignores.
//!
//! - ESLint 8 (`.eslintrc.*`, package.json `eslintConfig`): the config
//!   itself, then each of `overrides`. Configs it `extends` by path, or
//!   from a workspace package, are read in place. ESLint reads one per
//!   directory, and none where a flat config is in or above it.
//! - ESLint 9 flat configs (`eslint.config.*`): each element of the
//!   exported array, through `defineConfig`, `tseslint.config`,
//!   FlatCompat, spreads, `extends` and constants. Conditional elements
//!   are kept but marked; elements that can't be read, such as a shared
//!   config from another package, are opaque.
//! - oxlint: the configs it finds by itself (`.oxlintrc.json(c)`,
//!   `oxlint.config.(m)ts`), and any other a script passes to `--config`.
//!
//! A config file that exists but can't be read, as a YAML eslintrc, is
//! kept as opaque, as the linter uses it.

use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
};

use ignore::WalkBuilder;
use oxiclean_core::{Project, prefixed_package};
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

/// eslintrc file names, in ESLint 8's order; package.json's
/// `eslintConfig` comes last. YAML ones aren't read.
const ESLINTRC_FILES: &[&str] = &[
    ".eslintrc.js",
    ".eslintrc.cjs",
    ".eslintrc.yaml",
    ".eslintrc.yml",
    ".eslintrc.json",
    ".eslintrc",
    "package.json",
];

/// The oxlint config file names oxlint loads by itself, in order. Others
/// count only when a script passes them to `--config`.
const OXLINT_FILES: &[&str] =
    &[".oxlintrc.json", ".oxlintrc.jsonc", "oxlint.config.ts", "oxlint.config.mts"];

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Tool {
    /// ESLint 8's `.eslintrc` format.
    Eslintrc,
    /// ESLint 9's flat config.
    Flat,
    Oxlint,
}

impl Tool {
    /// The file names the linter looks for, in the order it prefers them.
    fn files(self) -> &'static [&'static str] {
        match self {
            Tool::Eslintrc => ESLINTRC_FILES,
            Tool::Flat => FLAT_FILES,
            Tool::Oxlint => OXLINT_FILES,
        }
    }

    /// Which linter looks for a file named `name`.
    fn of(name: &str) -> Option<Tool> {
        [Tool::Flat, Tool::Eslintrc, Tool::Oxlint].into_iter().find(|t| t.files().contains(&name))
    }
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

    fn opaque(files: Files, line: u32, conditional: bool) -> Self {
        Self { opaque: true, ..Self::new(files, line, conditional) }
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
    /// Whether it reads one of typescript-eslint's `configs`, which
    /// typescript-eslint 8.37 and later take to mean `tsconfigRootDir`
    /// is the config's directory.
    pub ts_configs: bool,
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
            ts_configs: false,
            root: false,
            type_aware: None,
            categories: Vec::new(),
            plugins: None,
            line: 1,
        }
    }

    /// A config the linter loads but that can't be read: it may set
    /// anything.
    fn unreadable(tool: Tool, path: &Path) -> Self {
        let mut c = Self::new(tool, path.to_path_buf(), None);
        c.entries.push(Entry::opaque(Files::All, 1, false));
        c.ignores_unknown = true;
        c
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
/// ignore and `node_modules`. Symbolic links to files count as the files.
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
        let is_file = entry
            .file_type()
            .is_some_and(|t| t.is_file() || (t.is_symlink() && entry.path().is_file()));
        if !is_file {
            continue;
        }
        let path = entry.into_path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        let in_test_inputs = || {
            path.strip_prefix(root).is_ok_and(|rel| {
                rel.components().any(|c| TEST_INPUT_DIRS.iter().any(|d| c.as_os_str() == *d))
            })
        };
        if matches!(name, "package.json" | "deno.json" | "deno.jsonc")
            && let Some(dir) = path.parent()
        {
            out.package_dirs.push(dir.to_path_buf());
        }
        if let Some(tool) = Tool::of(name)
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

/// Where a config file comes in its linter's order of file names.
fn rank(tool: Tool, path: &Path) -> usize {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    tool.files().iter().position(|f| *f == name).unwrap_or(usize::MAX)
}

/// Reads the configs the linters use of those `scan` found: the first in
/// each directory, in each linter's order, and no eslintrc where a flat
/// config is in or above its directory. `oxlint_extra` are configs
/// scripts pass to oxlint's `--config`. package.json files without
/// `eslintConfig` don't count.
pub(crate) fn read_all(
    project: &Project,
    scan: &Scan,
    oxlint_extra: &[PathBuf],
) -> Vec<LintConfig> {
    let resolver = Resolver { project };
    let mut candidates: Vec<&(Tool, PathBuf)> = scan.configs.iter().collect();
    candidates.sort_by_key(|(tool, path)| (*tool, path.parent(), rank(*tool, path)));
    let mut chosen: HashMap<(Tool, PathBuf), LintConfig> = HashMap::new();
    for (tool, path) in candidates {
        let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        if chosen.contains_key(&(*tool, dir.clone())) {
            continue;
        }
        let config = match tool {
            Tool::Flat => Some(read_flat(path, &resolver)),
            Tool::Eslintrc => read_eslintrc(path, &resolver),
            Tool::Oxlint => Some(read_oxlint(path, &resolver)),
        };
        if let Some(c) = config {
            chosen.insert((*tool, dir), c);
        }
    }
    let flat_dirs: Vec<PathBuf> =
        chosen.keys().filter(|(t, _)| *t == Tool::Flat).map(|(_, d)| d.clone()).collect();
    let mut out: Vec<LintConfig> = chosen
        .into_values()
        .filter(|c| {
            let dead = c.tool == Tool::Eslintrc && flat_dirs.iter().any(|d| c.dir.starts_with(d));
            if dead {
                debug!("{} is unused: a flat config is in or above it", c.path.display());
            }
            !dead
        })
        .collect();
    for path in oxlint_extra {
        if path.is_file() && !out.iter().any(|c| c.path == *path) {
            out.push(read_oxlint(path, &resolver));
        }
    }
    out.sort_by(|a, b| (a.tool, &a.path).cmp(&(b.tool, &b.path)));
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

/// Whether an eslintrc `extends` name is a shareable config package, as
/// opposed to a plugin's config or ESLint's own.
fn is_shareable(name: &str) -> bool {
    !name.starts_with("plugin:") && !name.starts_with("eslint:")
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
        None => c.entries.push(Entry::opaque(Files::All, 1, false)),
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
        self.config.entries.push(Entry::opaque(files.clone(), line, conditional));
    }

    fn elements(&mut self, items: &[Item], files: &Files, conditional: bool) {
        for i in items {
            self.element(&i.val, files, conditional, i.line);
        }
    }

    /// Notes what a reference to a package's export tells about the
    /// config as a whole.
    fn note_import(&mut self, source: &str, path: &str) {
        if source == "typescript-eslint" && path.starts_with("configs.") {
            self.config.ts_configs = true;
        }
        if source.contains("eslint-plugin-oxlint") {
            self.config.oxlint_plugin = true;
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
            Val::Call { callee, source, instance_of, args } => {
                let call = Call {
                    callee,
                    source: source.as_deref(),
                    instance_of: instance_of.as_deref(),
                    args,
                };
                self.call(&call, files, conditional, line);
            }
            Val::Import { source, path } => {
                self.note_import(source, path);
                if disables_types(path) {
                    let mut e = Entry::new(files.clone(), line, conditional);
                    e.typed = Some((Typed::Off, line));
                    self.config.entries.push(e);
                } else if sets_ts_parser(source, path) {
                    let mut e = Entry::new(files.clone(), line, conditional);
                    e.ts_parser = Some(true);
                    self.config.entries.push(e);
                } else if !known_package(source) && !source.contains("eslint-plugin-oxlint") {
                    self.opaque(files, conditional, line);
                }
            }
            _ => self.opaque(files, conditional, line),
        }
    }

    /// A call in the config array: a helper that returns its arguments as
    /// configs, `globalIgnores`, FlatCompat, or something that can't be
    /// read.
    fn call(&mut self, call: &Call<'_>, files: &Files, conditional: bool, line: u32) {
        let last = call.callee.rsplit('.').next().unwrap_or_default();
        let source = call.source.unwrap_or_default();
        let from_package = call.source.is_some_and(|s| !s.starts_with('.'));
        if source.contains("eslint-plugin-oxlint") {
            self.config.oxlint_plugin = true;
            return;
        }
        if let Some(class) = call.instance_of {
            if class == "FlatCompat" && source == "@eslint/eslintrc" {
                self.compat(last, call.args, files, conditional, line);
            } else {
                self.opaque(files, conditional, line);
            }
            return;
        }
        // `tseslint.config(...)` or `config(...)` imported by name.
        let helper = if call.callee.contains('.') {
            call.callee.split('.').count() == 2 && js::is_config_helper_export(source, last)
        } else {
            from_package && js::is_config_helper_export(source, last)
        };
        match last {
            _ if helper => self.elements(call.args, files, conditional),
            "fixupConfigRules" if from_package => self.elements(call.args, files, conditional),
            "globalIgnores" if from_package => {
                match call.args.first().and_then(|a| a.val.strings()) {
                    Some(patterns) => {
                        self.config.ignores.extend(patterns);
                        self.config.ignores_line = line;
                    }
                    None => self.config.ignores_unknown = true,
                }
            }
            "includeIgnoreFile" if from_package => self.config.gitignore = true,
            _ if source.contains("gitignore") => self.config.gitignore = true,
            _ => self.opaque(files, conditional, line),
        }
    }

    /// A method of FlatCompat, which turns eslintrc configs into flat
    /// ones: `compat.config({...})`, `compat.extends('plugin:x/y')`,
    /// `compat.plugins(...)` and `compat.env({...})`.
    fn compat(&mut self, method: &str, args: &[Item], files: &Files, conditional: bool, line: u32) {
        match method {
            "config" => {
                let Some(obj) = args.first().filter(|a| matches!(a.val, Val::Obj(_))) else {
                    self.opaque(files, conditional, line);
                    return;
                };
                let mut r = EslintrcReader::default();
                r.object(&obj.val, files, conditional, false);
                self.config.entries.extend(r.entries);
                self.config.ignores.extend(r.ignores);
                self.config.oxlint_plugin |= r.oxlint_plugin;
                self.config.ignores_unknown |= r.ignores_unknown;
            }
            "extends" => {
                let names: Vec<&str> = args.iter().filter_map(|a| a.val.as_str()).collect();
                if names.len() != args.len() {
                    self.opaque(files, conditional, line);
                    return;
                }
                for n in names {
                    self.extends_name(n, files, conditional, line);
                }
            }
            // Plugins and globals change nothing this reads.
            "plugins" | "env" => {}
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
        } else if is_shareable(name) && !known_package(&prefixed_package(name, "eslint-config")) {
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
            if let Val::Import { source, path } = &p.val {
                self.note_import(source, path);
            }
            match &p.val {
                Val::Import { path, .. } if disables_types(path) => {
                    e.typed = Some((Typed::Off, p.line));
                }
                Val::Import { source, path } if sets_ts_parser(source, path) => {
                    e.ts_parser = Some(true);
                }
                Val::Import { source, .. }
                    if known_package(source) || source.contains("eslint-plugin-oxlint") => {}
                _ => e.opaque = true,
            }
        }
        if v.get("languageOptions").is_some_and(|l| l.get(SPREAD).is_some()) {
            e.opaque = true;
        }
        self.config.entries.push(e);
    }
}

/// A call in a flat config, as [`Val::Call`] holds it.
struct Call<'v> {
    callee: &'v str,
    source: Option<&'v str>,
    instance_of: Option<&'v str>,
    args: &'v [Item],
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

/// Reads eslintrc objects into entries.
#[derive(Default)]
struct EslintrcReader {
    entries: Vec<Entry>,
    ignores: Vec<String>,
    oxlint_plugin: bool,
    ignores_unknown: bool,
}

impl EslintrcReader {
    fn opaque(&mut self, files: &Files, conditional: bool, line: u32) {
        self.entries.push(Entry::opaque(files.clone(), line, conditional));
        self.ignores_unknown = true;
    }

    /// The entries of eslintrc object `v`: itself, then each of
    /// `overrides`, whose files are relative to the config. The configs
    /// it extends are read by [`Self::extended`] when `extends_read`;
    /// otherwise shareable ones make it opaque.
    fn object(&mut self, v: &Val, files: &Files, conditional: bool, extends_read: bool) {
        let line = v.prop("extends").map_or(1, |p| p.line);
        let names = match v.get("extends").map(Val::strings) {
            None => Vec::new(),
            Some(Some(names)) => names,
            Some(None) => {
                self.opaque(files, conditional, line);
                Vec::new()
            }
        };
        for name in names {
            if name.starts_with("plugin:oxlint/") {
                self.oxlint_plugin = true;
            }
            if name == "plugin:@typescript-eslint/disable-type-checked" {
                let mut e = Entry::new(files.clone(), line, conditional);
                e.typed = Some((Typed::Off, line));
                self.entries.push(e);
            } else if eslintrc_sets_ts_parser(&name) {
                let mut e = Entry::new(files.clone(), line, conditional);
                e.ts_parser = Some(true);
                self.entries.push(e);
            } else if !extends_read
                && is_shareable(&name)
                && !known_package(&prefixed_package(&name, "eslint-config"))
            {
                self.opaque(files, conditional, line);
            }
        }
        if names_plugin(v.get("plugins"), "oxlint") {
            self.oxlint_plugin = true;
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
        self.entries.push(e);
        if let Some(p) = v.get("ignorePatterns") {
            match p.strings() {
                Some(patterns) => self.ignores.extend(patterns),
                None => self.ignores_unknown = true,
            }
        }
        if let Some(Val::Arr(overrides)) = v.get("overrides") {
            for o in overrides {
                self.override_entries(o, conditional);
            }
        }
    }

    /// The entries of one of `overrides`.
    fn override_entries(&mut self, o: &Item, conditional: bool) {
        let files = match o.val.get("files").map(Val::strings) {
            Some(Some(p)) => Files::Some(p),
            _ => Files::Unknown,
        };
        let mut inner = Self::default();
        inner.object(&o.val, &files, conditional, false);
        let excludes = o.val.get("excludedFiles").and_then(Val::strings).unwrap_or_default();
        for mut e in inner.entries {
            e.line = o.line.max(e.line);
            e.excludes.extend(excludes.iter().cloned());
            self.entries.push(e);
        }
        self.oxlint_plugin |= inner.oxlint_plugin;
        self.ignores_unknown |= inner.ignores_unknown;
    }

    /// Reads the configs eslintrc object `v` extends by path or from a
    /// workspace package, first, as ESLint merges them under it. Their
    /// entries keep the line of `extends` in the config being read. A
    /// shareable config that can't be found or read may set anything.
    fn extended(&mut self, v: &Val, dir: &Path, resolver: &Resolver<'_>, depth: usize) {
        let Some(line) = v.prop("extends").map(|p| p.line) else { return };
        for name in v.get("extends").and_then(Val::strings).unwrap_or_default() {
            let file = if name.starts_with('.') || name.starts_with('/') {
                js::resolve_path(&dir.join(&name))
            } else if is_shareable(&name) {
                let package = prefixed_package(&name, "eslint-config");
                if known_package(&package) {
                    continue;
                }
                resolver.resolve(dir, &package)
            } else {
                continue;
            };
            let Some(file) = file.filter(|_| depth <= 4) else {
                self.opaque(&Files::All, false, line);
                continue;
            };
            let loaded = js::load(&file, resolver);
            if loaded.packages.iter().any(|p| p.contains("eslint-plugin-oxlint")) {
                self.oxlint_plugin = true;
            }
            let Some(base) = loaded.value else {
                self.opaque(&Files::All, false, line);
                continue;
            };
            let base_dir = file.parent().map(Path::to_path_buf).unwrap_or_default();
            let mut inner = Self::default();
            inner.extended(&base, &base_dir, resolver, depth + 1);
            inner.object(&base, &Files::All, false, true);
            for mut e in inner.entries {
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
                self.entries.push(e);
            }
            self.ignores.extend(inner.ignores);
            self.oxlint_plugin |= inner.oxlint_plugin;
            self.ignores_unknown |= inner.ignores_unknown;
        }
    }
}

/// Whether eslintrc `plugins` names `name`.
fn names_plugin(plugins: Option<&Val>, name: &str) -> bool {
    plugins
        .and_then(Val::strings)
        .is_some_and(|p| p.iter().any(|p| p == name || p == &format!("eslint-plugin-{name}")))
}

/// Reads an eslintrc config, and the configs it extends in the project.
/// `None` for a package.json without `eslintConfig`; a config file that
/// can't be read, as a YAML one or JSON with a trailing comma (which
/// ESLint 8 rejects), is opaque.
fn read_eslintrc(path: &Path, resolver: &Resolver<'_>) -> Option<LintConfig> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    let unreadable = || Some(LintConfig::unreadable(Tool::Eslintrc, path));
    let (value, prefix, packages) = match name {
        "package.json" => {
            let manifest = js::strict_json_value(&std::fs::read_to_string(path).ok()?)?;
            (manifest.get("eslintConfig")?.clone(), Some(vec!["eslintConfig"]), Vec::new())
        }
        ".eslintrc.json" | ".eslintrc" => {
            let src = std::fs::read_to_string(path).ok()?;
            // `.eslintrc` is YAML, of which JSON is a part.
            let value = if name == ".eslintrc" {
                js::json_value(&src)
            } else {
                js::strict_json_value(&src)
            };
            match value {
                Some(v) => (v, Some(Vec::new()), Vec::new()),
                None => return unreadable(),
            }
        }
        ".eslintrc.js" | ".eslintrc.cjs" => {
            let loaded = js::load(path, resolver);
            match loaded.value {
                Some(v) => (v, None, loaded.packages),
                None => return unreadable(),
            }
        }
        _ => return unreadable(),
    };
    if !matches!(value, Val::Obj(_)) {
        return unreadable();
    }
    let mut c = LintConfig::new(Tool::Eslintrc, path.to_path_buf(), prefix);
    c.line = value.props().first().map_or(1, |p| p.line);
    c.root = value.get("root") == Some(&Val::Bool(true));
    c.ignores_line = value.prop("ignorePatterns").map_or(c.line, |p| p.line);
    c.oxlint_plugin = packages.iter().any(|p| p.contains("eslint-plugin-oxlint"));
    let mut r = EslintrcReader::default();
    r.extended(&value, &c.dir, resolver, 0);
    r.object(&value, &Files::All, false, true);
    c.entries = r.entries;
    c.ignores = r.ignores;
    c.oxlint_plugin |= r.oxlint_plugin;
    c.ignores_unknown = r.ignores_unknown;
    Some(c)
}

/// Reads an oxlint config, with the rules of the configs it extends; a
/// config that can't be read is opaque. A TS config may wrap its object
/// in oxlint's `defineConfig`.
fn read_oxlint(path: &Path, resolver: &Resolver<'_>) -> LintConfig {
    let value = match js::load(path, resolver).value {
        Some(Val::Call { callee, source, args, .. })
            if source.as_deref() == Some("oxlint")
                && callee.rsplit('.').next() == Some("defineConfig") =>
        {
            args.into_iter().next().filter(|a| !a.spread).map(|a| a.val)
        }
        v => v,
    };
    let Some(value) = value.filter(|v| matches!(v, Val::Obj(_))) else {
        return LintConfig::unreadable(Tool::Oxlint, path);
    };
    let json = path.extension().is_some_and(|e| e == "json" || e == "jsonc");
    let mut c = LintConfig::new(Tool::Oxlint, path.to_path_buf(), json.then(Vec::new));
    c.line = value.props().first().map_or(1, |p| p.line);
    c.ignores_line = value.prop("ignorePatterns").map_or(c.line, |p| p.line);
    let dir = c.dir.clone();
    read_oxlint_value(&mut c, &value, &dir, resolver, None, 0);
    if c.opaque() {
        c.ignores_unknown = true;
    }
    c
}

/// Reads oxlint config object `v`, from a file in `dir`, into `c`. `line`
/// is the line of `extends` that reaches it, for a config extended; such
/// a config's `ignorePatterns` don't apply, as oxlint doesn't inherit
/// them.
fn read_oxlint_value(
    c: &mut LintConfig,
    v: &Val,
    dir: &Path,
    resolver: &Resolver<'_>,
    line: Option<u32>,
    depth: usize,
) {
    if let Some(ext) = v.prop("extends") {
        let at = line.unwrap_or(ext.line);
        let items: Vec<&Val> = match &ext.val {
            Val::Arr(items) if items.iter().all(|i| !i.spread) => {
                items.iter().map(|i| &i.val).collect()
            }
            other => vec![other],
        };
        for item in items {
            match item {
                Val::Str(name) if depth < 4 => match js::resolve_path(&dir.join(name)) {
                    Some(f) => {
                        let base_dir = f.parent().map(Path::to_path_buf).unwrap_or_default();
                        match js::load(&f, resolver).value {
                            Some(base) => {
                                read_oxlint_value(
                                    c,
                                    &base,
                                    &base_dir,
                                    resolver,
                                    Some(at),
                                    depth + 1,
                                );
                            }
                            None => c.entries.push(Entry::opaque(Files::All, at, false)),
                        }
                    }
                    None => c.entries.push(Entry::opaque(Files::All, at, false)),
                },
                // An object the TS config imports or writes out.
                Val::Obj(_) if depth < 4 => {
                    read_oxlint_value(c, item, dir, resolver, Some(at), depth + 1);
                }
                _ => c.entries.push(Entry::opaque(Files::All, at, false)),
            }
        }
    }
    if line.is_none() {
        match v.get("ignorePatterns").map(Val::strings) {
            Some(Some(p)) => c.ignores.extend(p),
            Some(None) => c.ignores_unknown = true,
            None => {}
        }
    }
    let relined = |mut rules: Vec<RuleUse>| {
        if let Some(l) = line {
            for r in &mut rules {
                r.line = l;
            }
        }
        rules
    };
    let mut e = Entry::new(Files::All, line.unwrap_or(c.line), false);
    e.rules = relined(rules(v.get("rules")));
    c.entries.push(e);
    if let Some(Val::Arr(overrides)) = v.get("overrides") {
        for o in overrides {
            let files = match o.val.get("files").map(Val::strings) {
                Some(Some(p)) => Files::Some(p),
                _ => Files::Unknown,
            };
            let mut e = Entry::new(files, line.unwrap_or(o.line), false);
            e.rules = relined(rules(o.val.get("rules")));
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
            n if n.contains("oxlint") => read_oxlint(&path, &resolver),
            _ => read_eslintrc(&path, &resolver).unwrap(),
        }
    }

    #[test]
    fn reads_flat_entries() {
        let c = config(
            "eslint.config.js",
            "import tseslint from 'typescript-eslint';\nimport shared from '@acme/lint';\nexport default tseslint.config(\n  { ignores: ['dist/**'] },\n  { files: ['**/*.ts'], languageOptions: { parserOptions: { projectService: true } } },\n  { files: ['**/*.js'], ...tseslint.configs.disableTypeChecked },\n  { rules: { 'import/no-cycle': ['error', { maxDepth: 3 }], eqeqeq: 'off' } },\n  ...(process.env.CI ? [shared] : []),\n);\n",
        );
        assert_eq!(c.ignores, ["dist/**"]);
        assert_eq!(c.ignores_line, 4);
        assert!(c.ts_configs);
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
    fn reads_flat_compat_and_no_other_config_methods() {
        let c = config(
            "eslint.config.js",
            "import { FlatCompat } from '@eslint/eslintrc';\nconst compat = new FlatCompat({ baseDirectory: import.meta.dirname });\nexport default [\n  ...compat.config({ ignorePatterns: ['dist'], rules: { 'import/no-cycle': 'error' } }),\n  ...compat.extends('plugin:import/recommended'),\n];\n",
        );
        assert_eq!(c.ignores, ["dist"]);
        assert!(!c.opaque());
        assert_eq!(c.entries[0].rules[0].name, "import/no-cycle");
        // A `config` method of anything else may return anything.
        for src in [
            "import shared from './shared.js';\nexport default shared.config({ ignores: ['dist'] });\n",
            "const helpers = new Helpers();\nexport default helpers.config({ rules: {} });\n",
            "export default config({ rules: {} });\n",
        ] {
            assert!(config("eslint.config.js", src).opaque(), "{src}");
        }
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
    fn unreadable_eslintrc_is_opaque() {
        // Shareable configs that aren't in the project may set anything;
        // known ones don't.
        assert!(config(".eslintrc.json", r#"{ "extends": "@antfu" }"#).opaque());
        assert!(config(".eslintrc.json", r#"{ "extends": ["airbnb"] }"#).ignores_unknown);
        assert!(!config(".eslintrc.json", r#"{ "extends": ["prettier", "eslint:all"] }"#).opaque());
        // ESLint 8 rejects trailing commas in .eslintrc.json, not comments.
        assert!(config(".eslintrc.json", "{ \"rules\": {}, }").opaque());
        assert!(!config(".eslintrc.json", "{ // c\n \"rules\": {} }").opaque());
        assert!(config(".eslintrc.yml", "rules: {}\n").opaque());
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

    #[test]
    fn reads_oxlint_extends() {
        let t = tempfile::TempDir::new().unwrap();
        fs::write(
            t.path().join("base.ts"),
            "export default { ignorePatterns: ['gen'], rules: { 'typescript/no-floating-promises': 'error' } };\n",
        )
        .unwrap();
        fs::write(t.path().join("base.json"), r#"{ "ignorePatterns": ["out"] }"#).unwrap();
        let path = t.path().join("oxlint.config.ts");
        fs::write(
            &path,
            "import { defineConfig } from 'oxlint';\nimport base from './base.ts';\nexport default defineConfig({ extends: [base, './base.json'], ignorePatterns: ['x'] });\n",
        )
        .unwrap();
        let project = Project::new(t.path());
        let c = read_oxlint(&path, &Resolver { project: &project });
        assert!(!c.opaque());
        // Extended configs' ignores don't carry over.
        assert_eq!(c.ignores, ["x"]);
        assert_eq!(c.entries[0].rules[0].name, "typescript/no-floating-promises");
        assert_eq!(c.entries[0].rules[0].line, 3);
        let unknown = config(
            "oxlint.config.ts",
            "import base from '@acme/oxlint';\nexport default { extends: [base] };\n",
        );
        assert!(unknown.opaque() && unknown.ignores_unknown);
    }

    #[test]
    fn picks_one_config_per_directory() {
        let t = tempfile::TempDir::new().unwrap();
        let root = t.path();
        for (name, src) in [
            ("package.json", r#"{ "name": "x", "eslintConfig": { "root": true } }"#),
            (".eslintrc.json", "{}"),
            (".eslintrc.js", "module.exports = {};"),
            ("pkg/.eslintrc.yml", "root: true\n"),
            ("pkg/.eslintrc.json", "{}"),
            ("flat/eslint.config.js", "export default [];"),
            ("flat/sub/.eslintrc.json", "{}"),
        ] {
            let p = root.join(name);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, src).unwrap();
        }
        let project = Project::new(root);
        let configs = read_all(&project, &scan(root), &[]);
        let names: Vec<String> = configs
            .iter()
            .map(|c| c.path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/"))
            .collect();
        assert_eq!(names, [".eslintrc.js", "pkg/.eslintrc.yml", "flat/eslint.config.js"]);
    }
}
