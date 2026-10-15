//! `lint-config`: ESLint and oxlint configs that make linting slower than
//! it needs to be. Configs are read without running them (see
//! [`crate::configs`]). Reported, as `code`:
//!
//! - `typed-js-files`: type-aware parsing (`parserOptions.project` or
//!   `projectService`) applies to JavaScript files, such as
//!   `eslint.config.js`, so typescript-eslint type-checks them, or fails
//!   to parse those no tsconfig includes.
//! - `project-globs`: `parserOptions.project` names several tsconfigs, so
//!   typescript-eslint builds a program for each.
//! - `project-service`: `project: true` where typescript-eslint 8's
//!   `projectService` is available.
//! - `tsconfig-root-dir`: relative `project` paths without
//!   `tsconfigRootDir`, which resolve from wherever ESLint runs, unless
//!   typescript-eslint infers it.
//! - `slow-rule`: rules that walk the import graph, such as
//!   `import/no-cycle`, each with what does its job faster.
//! - `eslint-plugin-oxlint`: oxlint runs too, and ESLint still runs the
//!   rules oxlint covers.
//! - `build-output`: build output nothing ignores, so the linter parses
//!   generated files. `--fix` adds them to the ignores of JSON configs,
//!   and to the global ignores of flat configs whose exported array is
//!   written out in the file.
//! - `type-aware-unused`: oxlint's type-aware mode is on but no
//!   type-aware rule is. `--fix` turns `options.typeAware` off.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    path::{Path, PathBuf},
};

use oxiclean_core::{
    CheckResult, Finding, FindingKind, Issue, Project, Related, Result, Severity, Workspace,
    fix::{Fix, FixOp},
    jsonc_tree::Segment,
};
use oxiclean_typecheck::TsConfigs;
use serde_json::{Value, json};
use tracing::info;

use crate::{
    config::{Config, DEFAULT_THRESHOLD, LINT_CONFIG_RULE},
    configs::{self, Entry, Files, LintConfig, ProjectOption, Scan, Tool, Typed},
    js::{self, FixSite, Val},
    outputs::{self, GitIgnores, Globs, OutputDir, Why},
};

/// Rules that walk the import graph from every file, with what does the
/// same job faster.
const SLOW_RULES: &[(&str, &str)] = &[
    (
        "no-cycle",
        "follows every import of every file, parsing each module it reaches, on each run; `oxiclean cycles` finds the same cycles across the repo in one pass, and oxlint's import/no-cycle runs in the same pass as its other rules",
    ),
    (
        "no-unused-modules",
        "reads the exports and imports of every file in the project before it reports anything; `oxiclean unused` finds unused exports across the repo in one pass",
    ),
    (
        "namespace",
        "parses every module imported with `import * as` to check the names used on it; TypeScript checks the same in .ts files, and oxlint's import/namespace runs in the same pass as its other rules",
    ),
    (
        "no-deprecated",
        "parses every imported module for @deprecated tags; with type-aware linting, @typescript-eslint/no-deprecated reads them from the types it already has",
    ),
];

/// The import plugins whose rules [`SLOW_RULES`] names.
const IMPORT_PLUGINS: &[&str] = &["import", "import-x"];

/// oxlint's type-aware rules, with their categories, as of oxlint 1.x.
const TYPE_AWARE_RULES: &[(&str, &str)] = &[
    ("await-thenable", "correctness"),
    ("consistent-return", "suspicious"),
    ("consistent-type-exports", "style"),
    ("dot-notation", "style"),
    ("no-array-delete", "correctness"),
    ("no-base-to-string", "correctness"),
    ("no-confusing-void-expression", "pedantic"),
    ("no-deprecated", "pedantic"),
    ("no-duplicate-type-constituents", "correctness"),
    ("no-floating-promises", "correctness"),
    ("no-for-in-array", "correctness"),
    ("no-generated-empty-object-type", "suspicious"),
    ("no-implied-eval", "correctness"),
    ("no-meaningless-void-operator", "correctness"),
    ("no-misused-promises", "pedantic"),
    ("no-misused-spread", "correctness"),
    ("no-mixed-enums", "pedantic"),
    ("no-redundant-type-constituents", "correctness"),
    ("no-unnecessary-boolean-literal-compare", "suspicious"),
    ("no-unnecessary-condition", "nursery"),
    ("no-unnecessary-qualifier", "style"),
    ("no-unnecessary-template-expression", "suspicious"),
    ("no-unnecessary-type-arguments", "suspicious"),
    ("no-unnecessary-type-assertion", "suspicious"),
    ("no-unnecessary-type-conversion", "suspicious"),
    ("no-unnecessary-type-parameters", "suspicious"),
    ("no-unsafe-argument", "pedantic"),
    ("no-unsafe-assignment", "pedantic"),
    ("no-unsafe-call", "pedantic"),
    ("no-unsafe-enum-comparison", "suspicious"),
    ("no-unsafe-member-access", "pedantic"),
    ("no-unsafe-return", "pedantic"),
    ("no-unsafe-type-assertion", "suspicious"),
    ("no-unsafe-unary-minus", "correctness"),
    ("no-useless-default-assignment", "correctness"),
    ("non-nullable-type-assertion-style", "restriction"),
    ("only-throw-error", "pedantic"),
    ("prefer-find", "style"),
    ("prefer-includes", "pedantic"),
    ("prefer-nullish-coalescing", "pedantic"),
    ("prefer-optional-chain", "nursery"),
    ("prefer-promise-reject-errors", "pedantic"),
    ("prefer-readonly", "style"),
    ("prefer-readonly-parameter-types", "pedantic"),
    ("prefer-reduce-type-parameter", "style"),
    ("prefer-regexp-exec", "style"),
    ("prefer-return-this-type", "style"),
    ("prefer-string-starts-ends-with", "style"),
    ("promise-function-async", "restriction"),
    ("related-getter-setter-pairs", "pedantic"),
    ("require-array-sort-compare", "correctness"),
    ("require-await", "pedantic"),
    ("restrict-plus-operands", "pedantic"),
    ("restrict-template-expressions", "correctness"),
    ("return-await", "pedantic"),
    ("strict-boolean-expressions", "pedantic"),
    ("strict-void-return", "pedantic"),
    ("switch-exhaustiveness-check", "pedantic"),
    ("unbound-method", "correctness"),
    ("use-unknown-in-catch-callback-variable", "restriction"),
];

/// How many examples a message lists.
const EXAMPLES: usize = 3;

/// Runs the `lint-config` check with `cfg`.
///
/// # Errors
/// Fails if the root can't be found or read, or the config file is invalid.
pub fn run_lint_config_check(cfg: &Config) -> Result<CheckResult> {
    run_on(&cfg.common.workspace()?)
}

/// Runs the check on `workspace`.
pub(crate) fn run_on(workspace: &Workspace) -> Result<CheckResult> {
    info!("Starting {LINT_CONFIG_RULE} check");
    let policy = workspace.policy(LINT_CONFIG_RULE, DEFAULT_THRESHOLD, Severity::Warn)?;
    let (findings, configs) = check(&workspace.project);
    info!("{LINT_CONFIG_RULE} check complete. Found {} findings", findings.len());
    Ok(workspace.finish(&policy, findings, configs))
}

/// The findings for `project`, and how many lint configs it has.
pub(crate) fn check(project: &Project) -> (Vec<Finding>, usize) {
    let root = &project.root;
    let scan = configs::scan(root);
    let scripts = Scripts::read(&scan);
    let configs = configs::read_all(project, &scan, &scripts.oxlint_configs);
    if configs.is_empty() {
        return (Vec::new(), 0);
    }
    let tsconfigs = TsConfigs::load(project);
    let mut gitignores = GitIgnores::new(root);
    let outputs = outputs::output_dirs(root, &scan.package_dirs, &tsconfigs, &mut gitignores);
    let mut by_dir: [HashMap<PathBuf, usize>; 3] = Default::default();
    for (i, c) in configs.iter().enumerate() {
        by_dir[c.tool as usize].entry(c.dir.clone()).or_insert(i);
    }
    let mut cwds = vec![root.clone()];
    cwds.extend(scripts.eslint_dirs.iter().cloned());
    cwds.sort();
    cwds.dedup();
    let mut ctx = Ctx {
        project,
        scan: &scan,
        configs: &configs,
        scripts: &scripts,
        gitignores,
        globs: Globs::default(),
        tsconfigs: &tsconfigs,
        outputs: &outputs,
        by_dir,
        lookup: flat_lookup(project, &scripts),
        cwds,
    };
    let mut typed = typed_js_files(&mut ctx);
    let mut findings = Vec::new();
    for (i, c) in configs.iter().enumerate() {
        if c.tool != Tool::Oxlint {
            findings.extend(typed.remove(&i));
            findings.extend(project_option(&mut ctx, c));
            findings.extend(slow_rules(c));
            findings.extend(oxlint_plugin(&mut ctx, c));
        }
    }
    findings.extend(build_output(&mut ctx));
    findings.extend(type_aware_unused(&ctx));
    (findings, configs.len())
}

/// Which flat config ESLint uses for a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lookup {
    /// The one nearest the file, as ESLint 10 (or 9 with the
    /// `v10_config_lookup_from_file` flag) looks it up.
    PerFile,
    /// The one nearest the directory ESLint runs in, as ESLint 9 does.
    Cwd,
    /// ESLint's version isn't known: only a config both ways agree on.
    Unknown,
}

/// How the project's ESLint looks up flat configs.
fn flat_lookup(project: &Project, scripts: &Scripts) -> Lookup {
    if scripts.config_lookup_flag {
        return Lookup::PerFile;
    }
    match package_version(project, &project.root, &["eslint"]) {
        Some(v) if v.major >= 10 => Lookup::PerFile,
        Some(_) => Lookup::Cwd,
        None => Lookup::Unknown,
    }
}

/// What the checks share.
struct Ctx<'a> {
    project: &'a Project,
    scan: &'a Scan,
    configs: &'a [LintConfig],
    scripts: &'a Scripts,
    gitignores: GitIgnores,
    globs: Globs,
    tsconfigs: &'a TsConfigs,
    /// The project's build output directories.
    outputs: &'a [OutputDir],
    /// Each linter's config in each directory, by [`Tool`].
    by_dir: [HashMap<PathBuf, usize>; 3],
    lookup: Lookup,
    /// Where ESLint runs: the root, and directories whose scripts run it.
    cwds: Vec<PathBuf>,
}

impl<'a> Ctx<'a> {
    /// Whether `path` is in a build output directory.
    fn in_output(&self, path: &Path) -> bool {
        self.outputs.iter().any(|d| path.starts_with(&d.path))
    }

    /// `dir` and the directories above it, up to the root.
    fn up_from<'p>(&self, dir: &'p Path) -> impl Iterator<Item = &'p Path> {
        let root = &self.project.root;
        dir.ancestors().take_while(move |d| d.starts_with(root))
    }

    /// The config of `tool` in `dir`.
    fn at(&self, tool: Tool, dir: &Path) -> Option<usize> {
        self.by_dir[tool as usize].get(dir).copied()
    }

    /// The config of `tool` nearest above `path`, not counting one in
    /// `path` itself.
    fn nearest(&self, tool: Tool, path: &Path) -> Option<usize> {
        self.up_from(path.parent()?).find_map(|d| self.at(tool, d))
    }

    /// The configs of `tool` that apply to `path` as ESLint or oxlint
    /// runs.
    fn owners(&self, tool: Tool, path: &Path) -> Vec<usize> {
        let nearest = self.nearest(tool, path);
        if tool != Tool::Flat || self.lookup == Lookup::PerFile {
            return nearest.into_iter().collect();
        }
        let mut out: Vec<usize> = Vec::new();
        for cwd in &self.cwds {
            if !path.starts_with(cwd) || path == cwd {
                continue;
            }
            if let Some(i) = self.up_from(cwd).find_map(|d| self.at(Tool::Flat, d))
                && !out.contains(&i)
            {
                out.push(i);
            }
        }
        if self.lookup == Lookup::Unknown {
            out.retain(|i| Some(*i) == nearest);
        }
        out
    }

    /// The eslintrc configs whose settings reach file `path`: the
    /// nearest, and those above it up to one with `root: true`, farthest
    /// first.
    fn eslintrc_chain(&self, path: &Path) -> Vec<&'a LintConfig> {
        let configs = self.configs;
        let mut chain = Vec::new();
        for d in path.parent().into_iter().flat_map(|p| self.up_from(p)) {
            let Some(i) = self.at(Tool::Eslintrc, d) else { continue };
            chain.push(&configs[i]);
            if configs[i].root {
                break;
            }
        }
        chain.reverse();
        chain
    }
}

/// The finding for `config`.
fn finding(config: &LintConfig, issue: Issue, value: usize) -> Finding {
    Finding::new(config.path.clone(), FindingKind::Issue(issue), value)
}

/// `n` and the noun, as `1 file` or `3 files`.
fn count(n: usize, noun: &str) -> String {
    if n == 1 { format!("1 {noun}") } else { format!("{n} {noun}s") }
}

/// Up to [`EXAMPLES`] of `items`, quoted and joined.
fn examples(items: &[String]) -> String {
    listed(&items.iter().map(|s| format!("`{s}`")).collect::<Vec<_>>())
}

/// Up to [`EXAMPLES`] of `items`, joined.
fn listed(items: &[String]) -> String {
    let mut out: Vec<String> = items.iter().take(EXAMPLES).cloned().collect();
    if items.len() > EXAMPLES {
        out.push(format!("and {} more", items.len() - EXAMPLES));
    }
    out.join(", ")
}

// ---------------------------------------------------------------------
// Type-aware parsing of JavaScript files.

/// Whether `entry`, of a config in `dir` read by `tool`, applies to
/// `file`: yes, no, or can't tell.
fn applies(globs: &Globs, tool: Tool, dir: &Path, entry: &Entry, file: &Path) -> Option<bool> {
    let Some(rel) = outputs::relative(dir, file) else { return Some(false) };
    let matches = |patterns: &[String]| match tool {
        Tool::Eslintrc => globs.eslintrc_matches(patterns, &rel),
        _ => globs.flat_matches(patterns, &rel),
    };
    if !entry.excludes.is_empty() && matches(&entry.excludes) {
        return Some(false);
    }
    match &entry.files {
        Files::All => Some(true),
        Files::Some(p) => Some(matches(p)),
        Files::Unknown => None,
    }
}

/// The line that turns type-aware parsing on for `file`, when the
/// entries of `chain` certainly do.
fn typed_line(globs: &Globs, tool: Tool, chain: &[&LintConfig], file: &Path) -> Option<u32> {
    let mut on: Option<u32> = None;
    // Whether typescript-eslint's parser reads the file, which alone acts
    // on `project`: not at first, as ESLint's own parser does; `None` when
    // unknown.
    let mut ts_parser = Some(false);
    for c in chain {
        for e in &c.entries {
            let could_turn_off =
                e.opaque || matches!(e.typed, Some((Typed::Off | Typed::Unknown, _)));
            let parser_differs = e.opaque || e.ts_parser.is_some_and(|p| Some(p) != ts_parser);
            match applies(globs, tool, &c.dir, e, file) {
                Some(false) => continue,
                None => {
                    if could_turn_off {
                        on = None;
                    }
                    if parser_differs {
                        ts_parser = None;
                    }
                    continue;
                }
                Some(true) => {}
            }
            if parser_differs {
                ts_parser = if e.conditional || e.opaque { None } else { e.ts_parser };
            }
            match e.typed {
                Some((Typed::On, line)) if !e.conditional && !e.opaque => on = Some(line),
                // A conditional `On` leaves the file as it was.
                Some((Typed::On, _)) if !e.opaque => {}
                _ if could_turn_off => on = None,
                _ => {}
            }
        }
    }
    on.filter(|_| ts_parser == Some(true))
}

/// Whether ESLint lints JavaScript file `file` with the configs of
/// `chain`: ESLint 8 lints `.js` files, ESLint 9 `.js`, `.mjs` and `.cjs`
/// files, and either lints others only when a `files` pattern names
/// them (in flat configs, one that doesn't end in `*`).
fn linted(globs: &Globs, tool: Tool, chain: &[&LintConfig], file: &Path) -> bool {
    let ext = file.extension().and_then(|e| e.to_str()).unwrap_or_default();
    let by_default: &[&str] = match tool {
        Tool::Eslintrc => &["js"],
        _ => &["js", "mjs", "cjs"],
    };
    if by_default.contains(&ext) {
        return true;
    }
    chain.iter().any(|c| {
        let Some(rel) = outputs::relative(&c.dir, file) else { return false };
        c.entries.iter().any(|e| {
            let Files::Some(patterns) = &e.files else { return false };
            match tool {
                Tool::Eslintrc => applies(globs, tool, &c.dir, e, file) == Some(true),
                _ => patterns
                    .iter()
                    .any(|p| !p.ends_with('*') && globs.flat_pattern_matches(p, &rel)),
            }
        })
    })
}

/// Whether `config` (or the configs above it) ignores `path`.
fn ignores_path(ctx: &mut Ctx<'_>, config: &LintConfig, path: &Path, is_dir: bool) -> bool {
    if ctx.scripts.ignores(&ctx.globs, path) {
        return true;
    }
    match config.tool {
        Tool::Flat => {
            if config.gitignore && ctx.gitignores.ignored(path, is_dir) {
                return true;
            }
            let Some(rel) = outputs::relative(&config.dir, path) else { return false };
            if is_dir {
                ctx.globs.flat_ignores(&config.ignores, &rel)
            } else {
                ctx.globs.flat_matches(&config.ignores, &rel)
                    || rel
                        .rsplit_once('/')
                        .is_some_and(|(d, _)| ctx.globs.flat_ignores(&config.ignores, d))
            }
        }
        Tool::Eslintrc | Tool::Oxlint => {
            let configs = ctx.configs;
            let above = ctx.up_from(path).filter_map(|d| ctx.at(config.tool, d));
            let ignored = above
                .map(|i| &configs[i])
                .any(|c| ctx.globs.gitignore_style_ignores(&c.dir, &c.ignores, path));
            ignored
                || match config.tool {
                    Tool::Oxlint => ctx.gitignores.ignored(path, is_dir),
                    _ => {
                        ctx.gitignores.eslintignored(path, is_dir)
                            || in_dot_dir(&config.dir, path, is_dir)
                    }
                }
        }
    }
}

/// Whether `path` is in a directory whose name starts with a dot below
/// `dir`, or is one when `is_dir`: ESLint 8 skips those.
fn in_dot_dir(dir: &Path, path: &Path, is_dir: bool) -> bool {
    path.strip_prefix(dir).is_ok_and(|rel| {
        let dirs = if is_dir { Some(rel) } else { rel.parent() };
        dirs.is_some_and(|p| {
            p.components().any(|c| c.as_os_str().to_string_lossy().starts_with('.'))
        })
    })
}

/// JavaScript files ESLint configs parse with type information: a
/// finding for each config that does, by its index.
fn typed_js_files(ctx: &mut Ctx<'_>) -> HashMap<usize, Finding> {
    let configs = ctx.configs;
    let mut by_config: BTreeMap<usize, BTreeMap<u32, Vec<String>>> = BTreeMap::new();
    for f in &ctx.scan.js_files {
        if ctx.in_output(f) {
            continue;
        }
        for tool in [Tool::Eslintrc, Tool::Flat] {
            for i in ctx.owners(tool, f) {
                let config = &configs[i];
                let chain = match tool {
                    Tool::Eslintrc => ctx.eslintrc_chain(f),
                    _ => vec![config],
                };
                if !linted(&ctx.globs, tool, &chain, f) {
                    continue;
                }
                let Some(line) = typed_line(&ctx.globs, tool, &chain, f) else { continue };
                if ignores_path(ctx, config, f, false) {
                    continue;
                }
                let rel = outputs::relative(&config.dir, f).unwrap_or_default();
                by_config.entry(i).or_default().entry(line).or_default().push(rel);
            }
        }
    }
    let mut out = HashMap::new();
    for (i, by_line) in by_config {
        let Some((line, files)) = by_line.into_iter().max_by_key(|(_, f)| f.len()) else {
            continue;
        };
        let message = format!(
            "turns on type-aware parsing for {} too, such as {}, so typescript-eslint type-checks them along with the TypeScript files, or fails to parse those no tsconfig includes; set it in an entry with `files` for TypeScript only, or add typescript-eslint's disableTypeChecked for JavaScript files",
            count(files.len(), "JavaScript file"),
            examples(&files)
        );
        let issue = Issue::new("typed-js-files", "parserOptions", message, line);
        out.insert(i, finding(&configs[i], issue, files.len()));
    }
    out
}

// ---------------------------------------------------------------------
// `parserOptions.project`.

/// A package's version, installed or as a dependency range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Version {
    major: u64,
    minor: u64,
    /// Whether it is a range that may install a later minor version.
    floating: bool,
}

impl Version {
    /// Whether it is, or may be, at least `major.minor`.
    fn may_be_at_least(self, major: u64, minor: u64) -> bool {
        self.major > major || (self.major == major && (self.minor >= minor || self.floating))
    }
}

/// The version of the first of `packages` the package in `dir` uses,
/// installed or declared in package.json or deno.json, looking up to the
/// root.
fn package_version(project: &Project, dir: &Path, packages: &[&str]) -> Option<Version> {
    let mut cur = Some(dir);
    while let Some(d) = cur {
        for p in packages {
            let installed = d.join("node_modules").join(p);
            if let Some(v) = project
                .package_json(&installed)
                .and_then(|m| m.get("version").and_then(Value::as_str).and_then(version))
            {
                return Some(Version { floating: false, ..v });
            }
        }
        if let Some(m) = project.package_json(d) {
            for field in ["devDependencies", "dependencies"] {
                for p in packages {
                    if let Some(v) = m
                        .get(field)
                        .and_then(|f| f.get(*p))
                        .and_then(Value::as_str)
                        .and_then(version)
                    {
                        return Some(v);
                    }
                }
            }
        }
        for name in ["deno.json", "deno.jsonc"] {
            let Some(deno) =
                fs::read_to_string(d.join(name)).ok().and_then(|s| oxiclean_core::parse_jsonc(&s))
            else {
                continue;
            };
            for p in packages {
                let spec = deno.get("imports").and_then(|i| i.get(*p)).and_then(Value::as_str);
                if let Some(v) = spec
                    .and_then(|s| s.strip_prefix("npm:"))
                    .and_then(|s| s.strip_prefix(*p))
                    .and_then(|s| s.strip_prefix('@'))
                    .and_then(version)
                {
                    return Some(v);
                }
            }
        }
        if d == project.root {
            break;
        }
        cur = d.parent();
    }
    None
}

/// The version a version or range such as `^8.1.0` names, at least.
fn version(range: &str) -> Option<Version> {
    let rest = range.trim_start_matches(['^', '~', '=', '>', 'v', ' ']);
    if !rest.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    let mut parts = rest.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor_part = parts.next().unwrap_or("x");
    let minor = minor_part.parse().ok();
    let floating = range.trim_start().starts_with(['^', '>']) || minor.is_none();
    Some(Version { major, minor: minor.unwrap_or(0), floating })
}

/// The tsconfigs `patterns` name, relative to `dir`.
fn matched_tsconfigs(ctx: &Ctx<'_>, dir: &Path, patterns: &[String]) -> Vec<PathBuf> {
    let mut out = BTreeSet::new();
    for p in patterns {
        let p = p.trim_start_matches("./");
        if p.contains(['*', '?', '{', '[']) {
            let Some(m) = ctx.globs.glob(p) else { continue };
            out.extend(
                ctx.tsconfigs
                    .configs
                    .iter()
                    .map(|c| &c.path)
                    .filter(|c| outputs::relative(dir, c).is_some_and(|r| m.is_match(r)))
                    .cloned(),
            );
        } else {
            let path = path_clean(&dir.join(p));
            let path = if path.is_dir() { path.join("tsconfig.json") } else { path };
            out.insert(path);
        }
    }
    out.into_iter().collect()
}

/// `p` without `.` and `..` components.
fn path_clean(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    out
}

/// Packages that give typescript-eslint's version.
const TYPESCRIPT_ESLINT: &[&str] =
    &["typescript-eslint", "@typescript-eslint/parser", "@typescript-eslint/eslint-plugin"];

/// Findings about `parserOptions.project` in `config`.
fn project_option(ctx: &mut Ctx<'_>, config: &LintConfig) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    let version = package_version(ctx.project, &config.dir, TYPESCRIPT_ESLINT);
    let major = version.map(|v| v.major);
    // From 8.37, reading one of typescript-eslint's `configs` in a flat
    // config sets `tsconfigRootDir` to the config's directory.
    let root_dir = config.entries.iter().any(|e| e.root_dir)
        || (config.tool == Tool::Flat
            && config.ts_configs
            && version.is_some_and(|v| v.may_be_at_least(8, 37)));
    let service = match major {
        Some(m) if m >= 8 => "projectService: true, which typescript-eslint 8 provides,",
        _ => "typescript-eslint 8's projectService: true",
    };
    for e in &config.entries {
        let Some((project, line)) = &e.project else { continue };
        if !seen.insert(*line) {
            continue;
        }
        match project {
            ProjectOption::Paths(paths) => {
                let n = matched_tsconfigs(ctx, &config.dir, paths).len();
                if n >= 2 {
                    let message = format!(
                        "sets parserOptions.project to {n} tsconfigs, so typescript-eslint builds a TypeScript program for each and checks every file against them in turn; {service} uses the tsconfig of each file, as editors do"
                    );
                    out.push(finding(
                        config,
                        Issue::new("project-globs", "project", message, *line),
                        n,
                    ));
                }
                let relative = paths.iter().any(|p| !p.starts_with('/'));
                if relative && !root_dir && config.json_prefix.is_none() {
                    let message = "sets parserOptions.project to relative paths without tsconfigRootDir, so they resolve from the directory ESLint runs in, and runs from a package or an editor build other programs or none; set tsconfigRootDir to the config's directory (import.meta.dirname or __dirname)";
                    out.push(finding(
                        config,
                        Issue::new("tsconfig-root-dir", "tsconfigRootDir", message, *line),
                        1,
                    ));
                }
            }
            ProjectOption::Nearest if major.is_some_and(|m| m >= 8) => {
                let message = "sets parserOptions.project: true, which looks up and builds a program for the tsconfig of each file; projectService: true does the same through TypeScript's project service, which typescript-eslint 8 recommends as faster and closer to the editor";
                out.push(finding(
                    config,
                    Issue::new("project-service", "project", message, *line),
                    1,
                ));
            }
            ProjectOption::Nearest => {}
        }
    }
    out
}

// ---------------------------------------------------------------------
// Slow rules.

/// The rules of [`SLOW_RULES`] `config` turns on.
fn slow_rules(config: &LintConfig) -> Vec<Finding> {
    let mut first: BTreeMap<String, u32> = BTreeMap::new();
    let mut off_for_all: BTreeMap<String, bool> = BTreeMap::new();
    for e in &config.entries {
        for r in &e.rules {
            let Some((plugin, name)) = r.name.split_once('/') else { continue };
            if !IMPORT_PLUGINS.contains(&plugin) || !SLOW_RULES.iter().any(|(n, _)| *n == name) {
                continue;
            }
            if r.on && !e.conditional {
                first.entry(r.name.clone()).or_insert(r.line);
                off_for_all.insert(r.name.clone(), false);
            } else if !r.on && e.files == Files::All && !e.conditional {
                off_for_all.insert(r.name.clone(), true);
            }
        }
    }
    first
        .into_iter()
        .filter(|(rule, _)| !off_for_all.get(rule).copied().unwrap_or(false))
        .map(|(rule, line)| {
            let name = rule.split_once('/').map_or(rule.as_str(), |(_, n)| n);
            let why = SLOW_RULES.iter().find(|(n, _)| *n == name).map_or("", |(_, w)| w);
            let message = format!("turns on {rule}, which {why}");
            finding(config, Issue::new("slow-rule", rule.as_str(), message, line), 1)
        })
        .collect()
}

// ---------------------------------------------------------------------
// ESLint next to oxlint.

/// Whether oxlint lints what ESLint `config` does: an oxlint config sits
/// in its directory or above, or below it where it doesn't ignore, or the
/// package.json of its directory or one above lists oxlint or runs it.
fn uses_oxlint(ctx: &mut Ctx<'_>, config: &LintConfig) -> bool {
    if ctx.scripts.oxlint_dirs.iter().any(|d| config.dir.starts_with(d)) {
        return true;
    }
    let below: Vec<PathBuf> = ctx
        .configs
        .iter()
        .filter(|c| c.tool == Tool::Oxlint)
        .filter_map(|c| {
            if config.dir.starts_with(&c.dir) {
                Some(config.dir.clone())
            } else {
                c.dir.starts_with(&config.dir).then(|| c.dir.clone())
            }
        })
        .collect();
    below.iter().any(|d| *d == config.dir || !ignores_path(ctx, config, d, true))
}

/// The oxlint config oxlint reads for what ESLint `config` lints: the
/// nearest in its directory or above, or else one below it.
fn oxlint_config_for<'a>(ctx: &Ctx<'a>, config: &LintConfig) -> Option<&'a LintConfig> {
    let configs = ctx.configs;
    ctx.up_from(&config.dir).find_map(|d| ctx.at(Tool::Oxlint, d)).map(|i| &configs[i]).or_else(
        || configs.iter().find(|c| c.tool == Tool::Oxlint && c.dir.starts_with(&config.dir)),
    )
}

/// ESLint configs without eslint-plugin-oxlint where oxlint runs too.
fn oxlint_plugin(ctx: &mut Ctx<'_>, config: &LintConfig) -> Option<Finding> {
    if config.oxlint_plugin || config.opaque() || !uses_oxlint(ctx, config) {
        return None;
    }
    let how = match (config.tool, oxlint_config_for(ctx, config)) {
        (Tool::Flat, Some(ox)) => {
            let rel = outputs::relative(&config.dir, &ox.path)
                .map_or_else(|| ox.path.display().to_string(), |r| format!("./{r}"));
            if ox.json_prefix.is_some() {
                format!("add `...oxlint.buildFromOxlintConfigFile('{rel}')` last")
            } else {
                format!(
                    "import the config of `{rel}` and add `...oxlint.buildFromOxlintConfig(config)` last"
                )
            }
        }
        (Tool::Flat, None) => "add `...oxlint.configs['flat/recommended']` last".to_string(),
        _ => "add \"plugin:oxlint/recommended\" last in extends".to_string(),
    };
    let message = format!(
        "runs alongside oxlint without eslint-plugin-oxlint, so ESLint still runs the rules oxlint already checks; {how} to turn them off in ESLint"
    );
    let issue = Issue::new("eslint-plugin-oxlint", "eslint-plugin-oxlint", message, config.line);
    Some(finding(config, issue, 1))
}

// ---------------------------------------------------------------------
// Build output.

/// Build output each config's linter walks into.
fn build_output(ctx: &mut Ctx<'_>) -> Vec<Finding> {
    let configs = ctx.configs;
    let mut by_config: BTreeMap<&Path, Vec<&OutputDir>> = BTreeMap::new();
    for d in ctx.outputs {
        for tool in [Tool::Eslintrc, Tool::Flat, Tool::Oxlint] {
            for i in ctx.owners(tool, &d.path) {
                let owner = &configs[i];
                let declared = matches!(d.why, Why::OutDir(..));
                let walked = match tool {
                    // oxlint skips what git ignores.
                    Tool::Oxlint => d.files > 0 && !d.gitignored,
                    _ => d.files > 0 || declared,
                };
                if !walked || owner.ignores_unknown || ignores_path(ctx, owner, &d.path, true) {
                    continue;
                }
                by_config.entry(&owner.path).or_default().push(d);
            }
        }
    }
    let mut out = Vec::new();
    for (path, dirs) in by_config {
        let Some(config) = configs.iter().find(|c| c.path == path) else { continue };
        let rels: Vec<String> =
            dirs.iter().filter_map(|d| outputs::relative(&config.dir, &d.path)).collect();
        let described: Vec<String> = dirs
            .iter()
            .zip(&rels)
            .map(|(d, rel)| match (&d.why, d.files) {
                (_, n) if n > 0 => format!("`{rel}` ({})", count(n, "file")),
                (Why::OutDir(..), _) => format!("`{rel}` (tsconfig output)"),
                _ => format!("`{rel}`"),
            })
            .collect();
        let linter = match config.tool {
            Tool::Oxlint => "oxlint",
            _ => "ESLint",
        };
        let message = format!(
            "doesn't ignore build output, so {linter} parses generated files in {}; add {} to its ignores",
            listed(&described),
            if rels.len() == 1 { "it" } else { "them" }
        );
        let related = dirs
            .iter()
            .filter_map(|d| match &d.why {
                Why::OutDir(tsconfig, line) => {
                    Some(Related::new(tsconfig.clone(), *line, "emits build output there"))
                }
                _ => None,
            })
            .collect();
        let patterns: Vec<String> = rels.iter().map(|r| format!("{r}/**")).collect();
        let mut issue = Issue::new(
            "build-output",
            rels.join(","),
            message,
            config.ignores_line.max(config.line),
        )
        .with_related(related);
        if let Some(fix) = ignores_fix(config, &patterns) {
            issue = issue.with_fix(fix);
        }
        out.push(finding(config, issue, rels.len()));
    }
    out
}

/// `patterns` that aren't among `existing`, quoted with `quote`; `None`
/// when all are.
fn new_patterns(patterns: &[String], existing: &[String]) -> Option<Vec<String>> {
    let new: Vec<String> = patterns.iter().filter(|p| !existing.contains(p)).cloned().collect();
    (!new.is_empty()).then_some(new)
}

/// A fix adding `patterns` to `config`'s ignores, when it can be made
/// safely. Patterns the ignores already have aren't added again, so a fix
/// that doesn't clear the finding isn't made twice.
fn ignores_fix(config: &LintConfig, patterns: &[String]) -> Option<Fix> {
    if let Some(prefix) = &config.json_prefix {
        return json_ignores_fix(config, prefix, patterns);
    }
    if config.tool != Tool::Flat {
        return None;
    }
    let src = fs::read_to_string(&config.path).ok()?;
    let (site, q) = js::flat_fix_site(&config.path, &src)?;
    let existing = match &site {
        FixSite::Append { existing, .. } => existing.as_slice(),
        FixSite::Insert { .. } => &[],
    };
    let patterns = new_patterns(patterns, existing)?;
    let quoted = patterns.iter().map(|p| format!("\"{p}\"")).collect::<Vec<_>>().join(", ");
    let list = patterns.iter().map(|p| format!("{q}{p}{q}")).collect::<Vec<_>>().join(", ");
    let (start, expected, text, description) = match site {
        FixSite::Insert { start, expected, indent, empty } => {
            let object = format!("{{ ignores: [{list}] }}");
            let text = match (indent, empty) {
                (_, true) => format!("{expected}{object}"),
                (Some(indent), false) => format!("{expected}\n{indent}{object},"),
                (None, false) => format!("{expected}{object}, "),
            };
            (start, expected, text, format!("add a global ignores entry for {quoted}"))
        }
        FixSite::Append { start, expected, empty, .. } => {
            let text =
                if empty { format!("{expected}{list}") } else { format!("{expected}, {list}") };
            (start, expected, text, format!("add {quoted} to the global ignores"))
        }
    };
    let end = start + expected.len();
    Some(Fix::new(
        config.path.clone(),
        description,
        vec![FixOp::Replace { start, end, expected, text }],
    ))
}

/// A fix appending `patterns` to `ignorePatterns` of a JSON eslintrc or
/// oxlint config.
fn json_ignores_fix(
    config: &LintConfig,
    prefix: &[&'static str],
    patterns: &[String],
) -> Option<Fix> {
    let key = "ignorePatterns";
    let src = fs::read_to_string(&config.path).ok()?;
    let doc = js::json_value(&src)?;
    let mut node = Some(&doc);
    for k in prefix {
        node = node.and_then(|n| n.get(k));
    }
    let existing = match node.and_then(|n| n.get(key)) {
        None => Vec::new(),
        // eslintrc allows a single string, which an append can't extend.
        Some(v) if !matches!(v, Val::Arr(_)) => return None,
        Some(v) => v.strings().unwrap_or_default(),
    };
    let patterns = new_patterns(patterns, &existing)?;
    let quoted = patterns.iter().map(|p| format!("\"{p}\"")).collect::<Vec<_>>().join(", ");
    let path: Vec<Segment> = prefix.iter().copied().chain([key]).map(Segment::from).collect();
    let ops = patterns
        .iter()
        .map(|p| FixOp::JsonAppend { path: path.clone(), value: json!(p), seed: Vec::new() })
        .collect();
    let target = prefix.iter().copied().chain([key]).collect::<Vec<_>>().join(".");
    Some(Fix::new(config.path.clone(), format!("add {quoted} to \"{target}\""), ops))
}

// ---------------------------------------------------------------------
// oxlint's type-aware mode.

/// Rules of oxlint's `eslint` plugin named as type-aware rules are: a
/// name without a plugin is theirs.
const ESLINT_NAMESAKES: &[&str] =
    &["no-implied-eval", "prefer-promise-reject-errors", "require-await"];

/// The type-aware rule `rule` names in an oxlint config, as oxlint reads
/// it: `typescript/x`, `@typescript-eslint/x`, `typescript-eslint/x`, or
/// `x` when the eslint plugin has no rule of that name.
fn type_aware_name(rule: &str) -> Option<&str> {
    let name = match rule.split_once('/') {
        Some(("typescript" | "@typescript-eslint" | "typescript-eslint", name)) => name,
        Some(_) => return None,
        None if ESLINT_NAMESAKES.contains(&rule) => return None,
        None => rule,
    };
    TYPE_AWARE_RULES.iter().any(|(n, _)| *n == name).then_some(name)
}

/// Whether oxlint `config` turns on type-aware rule `name` of
/// `category`.
fn type_aware_rule_on(config: &LintConfig, name: &str, category: &str) -> bool {
    let mut explicit = None;
    for e in &config.entries {
        for r in e.rules.iter().filter(|r| type_aware_name(&r.name) == Some(name)) {
            if r.on {
                return true;
            }
            if e.files == Files::All {
                explicit = Some(false);
            }
        }
    }
    if explicit == Some(false) {
        return false;
    }
    let plugin_on = config.plugins.as_ref().is_none_or(|p| p.iter().any(|p| p == "typescript"));
    let category_on = config
        .categories
        .iter()
        .rev()
        .find(|(c, _)| c == category || c == "all")
        .map_or(category == "correctness", |(_, on)| *on);
    plugin_on && category_on
}

/// Whether oxlint `config` may turn on a type-aware rule.
fn any_type_aware_rule(config: &LintConfig) -> bool {
    config.opaque()
        || TYPE_AWARE_RULES
            .iter()
            .any(|(name, category)| type_aware_rule_on(config, name, category))
}

/// oxlint's type-aware mode turned on with no type-aware rule on.
fn type_aware_unused(ctx: &Ctx<'_>) -> Vec<Finding> {
    let configs = ctx.configs;
    let oxlint: Vec<&LintConfig> = configs.iter().filter(|c| c.tool == Tool::Oxlint).collect();
    let mut out = Vec::new();
    let message = "turns on oxlint's type-aware mode, which builds TypeScript programs with tsgolint, but no type-aware rule is on, so the programs go unused; turn it off, or turn on the type-aware rules you want";
    let in_config: Vec<(&LintConfig, u32)> =
        oxlint.iter().filter_map(|c| Some((*c, c.type_aware?))).collect();
    if !in_config.is_empty() && !oxlint.iter().any(|c| any_type_aware_rule(c)) {
        for (c, line) in in_config {
            let mut issue = Issue::new("type-aware-unused", "typeAware", message, line);
            if c.json_prefix.is_some() {
                issue = issue.with_fix(Fix::json_set(
                    c.path.clone(),
                    &["options", "typeAware"],
                    json!(false),
                ));
            }
            out.push(finding(c, issue, 1));
        }
    }
    // A script runs oxlint with the config it finds from its directory;
    // with none, oxlint's defaults turn type-aware correctness rules on.
    for (file, name, line) in &ctx.scripts.type_aware {
        let dir = file.parent().unwrap_or(file);
        let Some(i) = ctx.up_from(dir).find_map(|d| ctx.at(Tool::Oxlint, d)) else { continue };
        if any_type_aware_rule(&configs[i]) {
            continue;
        }
        let issue = Issue::new("type-aware-unused", name.as_str(), message, *line);
        out.push(Finding::new(file.clone(), FindingKind::Issue(issue), 1));
    }
    out
}

// ---------------------------------------------------------------------
// package.json scripts.

/// What package.json scripts tell about how the linters run.
#[derive(Debug, Default)]
struct Scripts {
    /// Directories whose package.json lists oxlint or runs it.
    oxlint_dirs: Vec<PathBuf>,
    /// Directories whose package.json runs ESLint.
    eslint_dirs: Vec<PathBuf>,
    /// Config files scripts pass to oxlint's `--config`.
    oxlint_configs: Vec<PathBuf>,
    /// Whether a script turns on ESLint 9's lookup of flat configs from
    /// each file.
    config_lookup_flag: bool,
    /// Patterns scripts pass to `--ignore-pattern`, and the files they
    /// pass to `--ignore-path`, as gitignore patterns rooted at the
    /// script's package.
    ignores: Vec<(PathBuf, Vec<String>)>,
    /// Scripts that run oxlint with `--type-aware` and no rule, category
    /// or config flags: package.json, script name, line.
    type_aware: Vec<(PathBuf, String, u32)>,
}

impl Scripts {
    fn read(scan: &Scan) -> Self {
        let mut out = Self::default();
        for dir in &scan.package_dirs {
            let file = dir.join("package.json");
            let Some(src) = fs::read_to_string(&file).ok() else { continue };
            let Some(manifest) = js::json_value(&src) else { continue };
            for field in ["dependencies", "devDependencies"] {
                if manifest.get(field).is_some_and(|d| d.get("oxlint").is_some()) {
                    out.oxlint_dirs.push(dir.clone());
                }
            }
            for p in manifest.get("scripts").map(Val::props).unwrap_or_default() {
                let Some(cmd) = p.val.as_str() else { continue };
                out.read_script(dir, &file, &p.key, cmd, p.line);
            }
        }
        out
    }

    fn read_script(&mut self, dir: &Path, file: &Path, name: &str, cmd: &str, line: u32) {
        // `--flag v10_config_lookup_from_file`, or the same in
        // `ESLINT_FLAGS`; ESLint 9 before 9.30 called it
        // `unstable_config_lookup_from_file`.
        if cmd.contains("config_lookup_from_file") {
            self.config_lookup_flag = true;
        }
        for part in cmd.split(['&', '|', ';']) {
            let words: Vec<String> = part
                .split_whitespace()
                .map(|w| w.trim_matches(|c| c == '"' || c == '\'').to_string())
                .collect();
            let Some(tool) = words.iter().position(|w| {
                let w = w.rsplit('/').next().unwrap_or(w);
                w == "eslint" || w == "oxlint"
            }) else {
                continue;
            };
            let oxlint = words[tool].ends_with("oxlint");
            if oxlint {
                self.oxlint_dirs.push(dir.to_path_buf());
            } else {
                self.eslint_dirs.push(dir.to_path_buf());
            }
            self.read_args(dir, file, name, line, oxlint, &words[tool + 1..]);
        }
    }

    /// Reads the arguments of a script's `eslint` or `oxlint` command.
    fn read_args(
        &mut self,
        dir: &Path,
        file: &Path,
        name: &str,
        line: u32,
        oxlint: bool,
        args: &[String],
    ) {
        let mut patterns = Vec::new();
        for (i, arg) in args.iter().enumerate() {
            let (flag, value) = match arg.split_once('=') {
                Some((f, v)) => (f, Some(v.to_string())),
                None => (arg.as_str(), None),
            };
            let value = || value.clone().or_else(|| args.get(i + 1).cloned());
            match flag {
                "--ignore-pattern" => patterns.extend(value()),
                "--ignore-path" => {
                    if let Some(f) = value()
                        && let Ok(text) = fs::read_to_string(dir.join(f))
                    {
                        patterns.extend(text.lines().map(str::to_string));
                    }
                }
                "-c" | "--config" if oxlint => {
                    self.oxlint_configs.extend(value().map(|f| path_clean(&dir.join(f))));
                }
                _ => {}
            }
        }
        if !patterns.is_empty() {
            self.ignores.push((dir.to_path_buf(), patterns));
        }
        let rule_flags = args.iter().any(|a| {
            matches!(a.as_str(), "-A" | "-W" | "-D" | "--allow" | "--warn" | "--deny")
                || a.starts_with("--allow=")
                || a.starts_with("--warn=")
                || a.starts_with("--deny=")
                || a.starts_with("-c")
                || a.starts_with("--config")
        });
        if oxlint && args.iter().any(|a| a == "--type-aware") && !rule_flags {
            self.type_aware.push((file.to_path_buf(), name.to_string(), line));
        }
    }

    /// Whether a script's ignore patterns cover `path`.
    fn ignores(&self, globs: &Globs, path: &Path) -> bool {
        self.ignores.iter().any(|(dir, patterns)| {
            globs.gitignore_style_ignores(dir, patterns, path)
                || outputs::relative(dir, path)
                    .is_some_and(|rel| globs.flat_ignores(patterns, &rel))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_versions() {
        let v = |major, minor, floating| Some(Version { major, minor, floating });
        assert_eq!(version("^8.1.0"), v(8, 1, true));
        assert_eq!(version("~7.0.0"), v(7, 0, false));
        assert_eq!(version(">=6"), v(6, 0, true));
        assert_eq!(version("8.46.1"), v(8, 46, false));
        assert_eq!(version("catalog:"), None);
        assert_eq!(version("workspace:*"), None);
        assert_eq!(version("npm:typescript-eslint@8"), None);
        assert!(version("^8.0.0").unwrap().may_be_at_least(8, 37));
        assert!(!version("8.36.0").unwrap().may_be_at_least(8, 37));
    }

    #[test]
    fn reads_script_flags() {
        let mut s = Scripts::default();
        s.read_script(
            Path::new("/r"),
            Path::new("/r/package.json"),
            "lint",
            "eslint --ignore-pattern \"examples/\" . && oxlint --type-aware",
            3,
        );
        assert_eq!(s.oxlint_dirs, [PathBuf::from("/r")]);
        assert_eq!(s.eslint_dirs, [PathBuf::from("/r")]);
        assert_eq!(s.ignores, [(PathBuf::from("/r"), vec!["examples/".to_string()])]);
        assert_eq!(s.type_aware.len(), 1);
        assert!(s.ignores(&Globs::default(), Path::new("/r/examples/next/dist")));
    }

    /// `file:line code key` for each finding in a project of `files`.
    fn run(files: &[(&str, &str)]) -> Vec<String> {
        let t = tempfile::TempDir::new().unwrap();
        for (name, src) in files {
            let path = t.path().join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, src).unwrap();
        }
        let project = Project::new(t.path());
        check(&project)
            .0
            .iter()
            .map(|f| {
                let FindingKind::Issue(i) = &f.kind else { panic!("{f:?}") };
                let file = f.file.strip_prefix(t.path()).unwrap().to_string_lossy().into_owned();
                format!("{file}:{} {} {}", i.line, i.code, i.key)
            })
            .collect()
    }

    const PKG: &str = r#"{ "name": "x", "devDependencies": { "typescript-eslint": "^8.0.0" } }"#;

    #[test]
    fn skips_malformed_and_yaml_configs() {
        for (name, src) in [
            (".eslintrc.json", "{ \"rules\": { "),
            (".eslintrc.yml", "rules:\n  import/no-cycle: error\n"),
            ("eslint.config.js", "export default [ { rules: "),
            (".oxlintrc.json", "not json"),
            ("eslint.config.js", "export default 42;"),
            ("eslint.config.mjs", "export default [null, 1, 'x', [[]]];"),
        ] {
            assert!(run(&[("package.json", PKG), (name, src)]).is_empty(), "{name}: {src}");
        }
    }

    #[test]
    fn reads_json_with_comments_but_not_trailing_commas() {
        assert_eq!(
            run(&[
                ("package.json", PKG),
                (
                    ".eslintrc.json",
                    "{\n  // slow\n  \"rules\": {\n    \"import/no-cycle\": \"error\" /* x */\n  }\n}\n"
                ),
            ]),
            [".eslintrc.json:4 slow-rule import/no-cycle"]
        );
        // ESLint 8 can't read it with a trailing comma.
        assert!(
            run(&[
                ("package.json", PKG),
                (".eslintrc.json", "{ \"rules\": { \"import/no-cycle\": \"error\", } }"),
            ])
            .is_empty()
        );
    }

    #[test]
    fn follows_circular_local_extends() {
        let found = run(&[
            ("package.json", PKG),
            (".eslintrc.json", r#"{ "root": true, "extends": ["./base.json"] }"#),
            (
                "base.json",
                r#"{ "extends": "./.eslintrc.json", "rules": { "import/no-cycle": 2 } }"#,
            ),
        ]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("slow-rule import/no-cycle"), "{found:?}");
    }

    #[test]
    fn rules_turned_off_later_are_not_reported() {
        assert!(
            run(&[
                ("package.json", PKG),
                (
                    "eslint.config.js",
                    "export default [\n  { rules: { 'import/no-cycle': 'error' } },\n  { rules: { 'import/no-cycle': 'off' } },\n];\n"
                ),
            ])
            .is_empty()
        );
    }

    #[test]
    fn typed_js_needs_the_typescript_parser() {
        // `project` without typescript-eslint's parser does nothing.
        let flat = "export default [\n  { languageOptions: { parserOptions: { project: './tsconfig.json', tsconfigRootDir: import.meta.dirname } } },\n];\n";
        let files = [
            ("package.json", PKG),
            ("tsconfig.json", r#"{ "include": ["src"] }"#),
            ("src/a.js", "export {};\n"),
            ("eslint.config.js", flat),
        ];
        assert!(run(&files).iter().all(|f| !f.contains("typed-js-files")), "{:?}", run(&files));
        let with_parser = format!(
            "import tseslint from 'typescript-eslint';\n{}",
            flat.replace("languageOptions: {", "languageOptions: { parser: tseslint.parser,",)
        );
        let found = run(&[files[0], files[1], files[2], ("eslint.config.js", &with_parser)]);
        assert!(found.iter().any(|f| f.contains("typed-js-files")), "{found:?}");
    }

    #[test]
    fn mutated_configs_are_not_trusted() {
        // Something is pushed onto the array after it's declared, so its
        // global ignores aren't the whole story and there's nothing to fix.
        let found = run(&[
            ("package.json", r#"{ "name": "x" }"#),
            ("tsconfig.json", r#"{ "compilerOptions": { "outDir": "dist" } }"#),
            ("src/a.ts", "export {};\n"),
            ("dist/a.js", "export {};\n"),
            (
                "eslint.config.js",
                "import shared from './shared.js';\nconst config = [{ ignores: ['lib'] }];\nconfig.push(shared);\nexport default config;\n",
            ),
            ("shared.js", "export default { ignores: ['dist'] };\n"),
        ]);
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn spread_and_conditional_ignores_count() {
        let files = [
            ("package.json", r#"{ "name": "x" }"#),
            ("tsconfig.json", r#"{ "compilerOptions": { "outDir": "dist" } }"#),
            ("src/a.ts", "export {};\n"),
            ("dist/a.js", "export {};\n"),
        ];
        for config in [
            "import ignores from './ignores.js';\nexport default [ignores, { rules: {} }];\n",
            "const base = [{ ignores: ['dist/'] }];\nexport default [...base];\n",
            "export default [process.env.CI ? { ignores: ['dist'] } : {}];\n",
            "import { globalIgnores } from 'eslint/config';\nexport default [globalIgnores(['**/dist'])];\n",
        ] {
            let mut all = files.to_vec();
            all.push(("eslint.config.js", config));
            all.push(("ignores.js", "export default { ignores: ['dist/**'] };\n"));
            let found = run(&all);
            assert!(found.is_empty(), "{config}: {found:?}");
        }
        // Without ignores, the output is reported.
        let mut all = files.to_vec();
        all.push(("eslint.config.js", "export default [{ rules: {} }];\n"));
        assert_eq!(run(&all), ["eslint.config.js:1 build-output dist"]);
    }

    #[test]
    fn package_json_eslint_config_is_read() {
        let found = run(&[(
            "package.json",
            r#"{ "name": "x", "eslintConfig": { "rules": { "import/no-unused-modules": "warn" } } }"#,
        )]);
        assert_eq!(found, ["package.json:1 slow-rule import/no-unused-modules"]);
    }

    #[test]
    fn eslintrc_js_with_require_spread() {
        let found = run(&[
            ("package.json", PKG),
            ("base.js", "module.exports = { rules: { 'import/no-cycle': 'error' } };\n"),
            (
                ".eslintrc.js",
                "const base = require('./base');\nmodule.exports = { ...base, root: true };\n",
            ),
        ]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("slow-rule import/no-cycle"), "{found:?}");
    }

    #[test]
    fn opaque_shared_configs_suppress_findings() {
        // Ignores might come from the shared config.
        let found = run(&[
            ("package.json", r#"{ "name": "x" }"#),
            ("tsconfig.json", r#"{ "compilerOptions": { "outDir": "dist" } }"#),
            ("src/a.ts", "export {};\n"),
            ("dist/a.js", "export {};\n"),
            (
                "eslint.config.js",
                "import acme from '@acme/eslint-config';\nexport default [...acme];\n",
            ),
        ]);
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn oxlint_rule_names_are_read_as_oxlint_does() {
        let oxlintrc = |rule: &str| {
            format!(
                r#"{{ "categories": {{ "correctness": "off" }}, "rules": {{ "{rule}": "error" }}, "options": {{ "typeAware": true }} }}"#
            )
        };
        for rule in [
            "typescript-eslint/no-floating-promises",
            "no-floating-promises",
            "@typescript-eslint/no-floating-promises",
        ] {
            let found = run(&[("package.json", PKG), (".oxlintrc.json", &oxlintrc(rule))]);
            assert!(found.is_empty(), "{rule}: {found:?}");
        }
        // Without a plugin, `require-await` is the eslint plugin's.
        let found = run(&[("package.json", PKG), (".oxlintrc.json", &oxlintrc("require-await"))]);
        assert_eq!(found, [".oxlintrc.json:1 type-aware-unused typeAware"]);
    }

    #[test]
    fn type_aware_scripts_use_the_config_oxlint_finds() {
        let pkg = r#"{ "name": "x", "scripts": { "lint": "oxlint --type-aware" } }"#;
        // oxlint's defaults turn type-aware correctness rules on.
        assert!(run(&[("package.json", pkg), ("a.ts", "export {};\n")]).is_empty());
        let off = r#"{ "categories": { "correctness": "off" } }"#;
        assert_eq!(
            run(&[("package.json", pkg), (".oxlintrc.json", off)]),
            ["package.json:1 type-aware-unused lint"]
        );
        // oxlint doesn't load `oxlint.json` by itself.
        assert!(run(&[("package.json", pkg), ("oxlint.json", off)]).is_empty());
    }

    #[test]
    fn eslint_8_skips_dot_directories() {
        let found = run(&[
            ("package.json", r#"{ "name": "x", "devDependencies": { "eslint": "^8.57.0" } }"#),
            (".eslintrc.json", r#"{ "root": true }"#),
            (".next/server/page.js", "export {};\n"),
        ]);
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn eslintrc_next_to_a_flat_config_is_unused() {
        let found = run(&[
            ("package.json", r#"{ "name": "x", "type": "module" }"#),
            ("eslint.config.js", "export default [{ rules: {} }];\n"),
            ("pkg/.eslintrc.json", r#"{ "rules": { "import/no-cycle": "error" } }"#),
        ]);
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn unresolved_shareable_configs_may_ignore_output() {
        let found = run(&[
            ("package.json", r#"{ "name": "x" }"#),
            (".gitignore", "dist\n"),
            (".eslintrc.json", r#"{ "root": true, "extends": "@antfu" }"#),
            ("dist/a.js", "export {};\n"),
        ]);
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn typescript_eslint_configs_set_tsconfig_root_dir() {
        let config = |extra: &str| {
            format!(
                "import tseslint from 'typescript-eslint';\nexport default [\n  {extra}\n  {{ files: ['**/*.ts'], languageOptions: {{ parser: tseslint.parser, parserOptions: {{ project: ['./tsconfig.json'] }} }} }},\n];\n"
            )
        };
        let pkg = |v: &str| {
            format!(r#"{{ "name": "x", "devDependencies": {{ "typescript-eslint": "{v}" }} }}"#)
        };
        let root_dir = |pkg: &str, config: &str| {
            run(&[("package.json", pkg), ("tsconfig.json", "{}"), ("eslint.config.js", config)])
                .iter()
                .any(|f| f.contains("tsconfig-root-dir"))
        };
        assert!(root_dir(&pkg("^8.46.0"), &config("")));
        assert!(!root_dir(&pkg("^8.46.0"), &config("tseslint.configs.base,")));
        assert!(root_dir(&pkg("8.36.0"), &config("tseslint.configs.base,")));
    }

    #[test]
    fn jsx_is_linted_when_a_files_pattern_names_it() {
        let config = |files: &str| {
            format!(
                "import tseslint from 'typescript-eslint';\nexport default tseslint.config(\n  ...tseslint.configs.recommendedTypeChecked,\n  {{ {files}languageOptions: {{ parserOptions: {{ projectService: true, tsconfigRootDir: import.meta.dirname }} }} }},\n);\n"
            )
        };
        // Only eslint.config.js: `a.jsx` isn't linted.
        let files = |config: &str| {
            let t = tempfile::TempDir::new().unwrap();
            for (name, src) in
                [("package.json", PKG), ("a.jsx", "export {};\n"), ("eslint.config.js", config)]
            {
                fs::write(t.path().join(name), src).unwrap();
            }
            let project = Project::new(t.path());
            check(&project).0.iter().map(|f| f.value).sum::<usize>()
        };
        assert_eq!(files(&config("")), 1);
        assert_eq!(files(&config("files: ['**/*.{js,jsx}'], ")), 2);
    }

    #[test]
    fn nested_flat_configs_follow_the_eslint_version() {
        let files = |eslint: &str| {
            run(&[
                (
                    "package.json",
                    &format!(
                        r#"{{ "name": "x", "type": "module", "devDependencies": {{ "eslint": "{eslint}" }} }}"#
                    ),
                ),
                (".gitignore", "dist\n"),
                ("eslint.config.js", "export default [{ ignores: ['**/dist/'] }];\n"),
                ("app/package.json", r#"{ "name": "app", "type": "module" }"#),
                ("app/eslint.config.js", "export default [{ rules: {} }];\n"),
                ("app/dist/a.js", "export {};\n"),
            ])
        };
        // ESLint 9 uses the config of the directory it runs in.
        assert!(files("^9.39.0").is_empty());
        assert_eq!(files("^10.0.0"), ["app/eslint.config.js:1 build-output dist"]);
    }

    #[cfg(unix)]
    #[test]
    fn follows_symlinked_configs() {
        let t = tempfile::TempDir::new().unwrap();
        fs::create_dir_all(t.path().join("shared")).unwrap();
        fs::write(t.path().join("package.json"), PKG).unwrap();
        fs::write(
            t.path().join("shared/eslintrc.json"),
            r#"{ "root": true, "rules": { "import/no-cycle": "error" } }"#,
        )
        .unwrap();
        std::os::unix::fs::symlink("shared/eslintrc.json", t.path().join(".eslintrc.json"))
            .unwrap();
        let project = Project::new(t.path());
        assert_eq!(check(&project).0.len(), 1);
    }

    #[test]
    fn fixes_add_only_new_patterns() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(new_patterns(&s(&["a/**", "b/**"]), &s(&["a/**"])), Some(s(&["b/**"])));
        assert_eq!(new_patterns(&s(&["a/**"]), &s(&["a/**"])), None);
    }

    #[test]
    fn flat_compat_configs_get_no_fix() {
        let found = run(&[
            ("package.json", r#"{ "name": "x", "type": "module" }"#),
            (".gitignore", "dist\n"),
            ("dist/a.js", "export {};\n"),
            (
                "eslint.config.js",
                "import { FlatCompat } from '@eslint/eslintrc';\nconst compat = new FlatCompat();\nexport default compat.config({ rules: {} });\n",
            ),
        ]);
        assert_eq!(found, ["eslint.config.js:1 build-output dist"]);
    }
}
