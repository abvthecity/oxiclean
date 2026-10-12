use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

use globset::GlobSet;
use oxiclean_core::{
    Analysis, BuildOptions, CheckResult, Finding, FindingKind, Project, Result, RulePolicy,
    UnusedDependency, parse_jsonc, split_package_request,
};
use serde_json::Value;
use tracing::{debug, info};

use crate::{
    config::{Config, DEFAULT_THRESHOLD, UNUSED_DEPENDENCIES_RULE, UNUSED_DEV_DEPENDENCIES_RULE},
    scripts::{ScriptRef, is_bare, script_refs},
};

/// Packages that are never reported, as in knip: the compiler every
/// tsconfig needs, and knip itself.
const ALWAYS_USED: &[&str] = &["typescript", "knip"];

/// `@types/X` packages that are used without an import of `X`: they
/// declare globals.
const GLOBAL_TYPES: &[&str] = &["@types/node", "@types/bun", "@types/jest"];

/// Binaries whose package isn't named after them, for packages that aren't
/// installed. An installed package's `bin` field is used instead.
const KNOWN_BINARIES: &[(&str, &[&str])] = &[
    ("typescript", &["tsc", "tsserver"]),
    ("@biomejs/biome", &["biome"]),
    ("@playwright/test", &["playwright"]),
    ("@changesets/cli", &["changeset"]),
    ("@microsoft/api-extractor", &["api-extractor"]),
    ("@angular/cli", &["ng"]),
    ("@nestjs/cli", &["nest"]),
    ("@vue/cli-service", &["vue-cli-service"]),
    ("npm-run-all", &["npm-run-all", "run-p", "run-s"]),
    ("npm-run-all2", &["npm-run-all", "run-p", "run-s"]),
    ("@swc/cli", &["swc"]),
    ("@babel/cli", &["babel"]),
    ("@babel/node", &["babel-node"]),
    ("webpack-cli", &["webpack", "webpack-cli"]),
    ("storybook", &["storybook", "sb"]),
    ("@commitlint/cli", &["commitlint"]),
    ("jest-cli", &["jest"]),
    ("@rsbuild/core", &["rsbuild"]),
    ("@rspack/cli", &["rspack"]),
    ("@tailwindcss/cli", &["tailwindcss"]),
    ("@napi-rs/cli", &["napi"]),
    ("@sveltejs/kit", &["svelte-kit"]),
    ("@expo/cli", &["expo"]),
    ("dotenv-cli", &["dotenv"]),
];

/// The package.json fields that list dependencies.
const FIELDS: [Field; 4] =
    [Field::Dependencies, Field::DevDependencies, Field::Peer, Field::Optional];

/// A package.json field that lists packages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Field {
    Dependencies,
    DevDependencies,
    /// `peerDependencies`: never reported, as the package that installs
    /// this one provides them.
    Peer,
    /// `optionalDependencies`: never reported.
    Optional,
}

impl Field {
    /// The field's key in package.json.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Dependencies => "dependencies",
            Self::DevDependencies => "devDependencies",
            Self::Peer => "peerDependencies",
            Self::Optional => "optionalDependencies",
        }
    }

    /// The rule that reports packages in this field, if any.
    #[must_use]
    pub fn rule(self) -> Option<&'static str> {
        match self {
            Self::Dependencies => Some(UNUSED_DEPENDENCIES_RULE),
            Self::DevDependencies => Some(UNUSED_DEV_DEPENDENCIES_RULE),
            Self::Peer | Self::Optional => None,
        }
    }
}

/// Runs the unused dependencies check described by `cfg`.
///
/// # Errors
/// Fails if the project root can't be found, the config file is invalid, or
/// no entry files are found.
pub fn run_unused_dependencies_check(cfg: &Config) -> Result<CheckResult> {
    run(cfg, Field::Dependencies)
}

/// Runs the unused devDependencies check described by `cfg`.
///
/// # Errors
/// As for [`run_unused_dependencies_check`].
pub fn run_unused_dev_dependencies_check(cfg: &Config) -> Result<CheckResult> {
    run(cfg, Field::DevDependencies)
}

fn run(cfg: &Config, field: Field) -> Result<CheckResult> {
    let rule = field.rule().unwrap_or(UNUSED_DEPENDENCIES_RULE);
    info!("Starting {rule} check");
    // Type-only imports use packages too, so they are resolved and the
    // files only they reach are read.
    let analysis =
        cfg.common.analyze_package_entries(BuildOptions::default().resolve_passive(true))?;
    let policy = analysis.policy(rule, DEFAULT_THRESHOLD)?;
    let ignored = analysis.config.ignored_dependencies()?;
    Ok(check(&analysis, &policy, field, &ignored))
}

/// Reports each package in `field` of the root's or a workspace package's
/// package.json that nothing uses; see [`unused_dependencies`]. Packages
/// `ignored` matches are left out.
#[must_use]
pub fn check(
    analysis: &Analysis,
    policy: &RulePolicy,
    field: Field,
    ignored: &GlobSet,
) -> CheckResult {
    let rule = field.rule().unwrap_or(UNUSED_DEPENDENCIES_RULE);
    if policy.min_threshold().is_none() {
        info!("{rule} is disabled");
        return analysis.finish(policy, Vec::new());
    }
    let findings: Vec<Finding> = unused_dependencies(analysis)
        .into_iter()
        .filter(|u| u.field == field && !ignored.is_match(&u.name))
        .map(|u| {
            let dep = UnusedDependency::new(u.name, field.key().to_string(), u.line);
            Finding::new(u.manifest, FindingKind::UnusedDependency(dep), 1)
        })
        .collect();
    info!("{rule} check complete. Found {} unused", findings.len());
    analysis.finish(policy, findings)
}

/// A package a package.json lists that nothing uses.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Unused {
    /// The package.json.
    pub manifest: PathBuf,
    pub name: String,
    pub field: Field,
    /// 1-based line of the package in the package.json, or 0 when unknown.
    pub line: u32,
}

/// The packages in `dependencies` and `devDependencies` of the root's and
/// each workspace package's package.json that nothing uses, by package.json
/// and line, as knip's `dependencies` and `devDependencies` find them with
/// its plugins off. A package is used when:
///
/// - a file the package entries reach imports it, in any form: `import`,
///   `import type`, `require` or `import()`. The import counts for the
///   nearest package.json that lists the package, from the file's own
///   package up to the root, as Node's resolution finds it;
/// - a script in the package.json runs one of its binaries, through `npx`
///   or `pnpm exec` too, or loads it with `-r` or `--import`. Binaries are
///   matched to packages by the installed packages' `bin` field, or else by
///   name;
/// - a tsconfig.json or `tsconfig.*.json` beside the package.json names it
///   in `extends`, `compilerOptions.types`, `jsxImportSource` or `plugins`;
/// - a used package that is installed lists it in `peerDependencies`;
/// - it is `typescript`, or `@types/node`, `@types/bun` or `@types/jest`,
///   which declare globals.
///
/// `@types/X` (or `@types/scope__x` for `@scope/x`) is used when `X` is
/// imported or is in tsconfig `types`, unless `X` is installed and ships its
/// own types; running `X`'s binary doesn't use it. A package listed in both
/// `dependencies` and `devDependencies` is reported in `devDependencies`.
/// `peerDependencies` and `optionalDependencies` are never reported.
#[must_use]
pub fn unused_dependencies(analysis: &Analysis) -> Vec<Unused> {
    let project = &analysis.project;
    let manifests = Manifests::load(project);
    let mut used = Used::new(&manifests, project);

    let mut owners: HashMap<&Path, Option<usize>> = HashMap::new();
    for module in analysis.graph.modules() {
        if module.in_node_modules() || module.packages.is_empty() {
            continue;
        }
        let Some(dir) = module.path.parent() else { continue };
        let owner = *owners.entry(dir).or_insert_with(|| manifests.owner(dir));
        let Some(owner) = owner else { continue };
        for request in &module.packages {
            used.import(owner, request);
        }
    }
    for (i, manifest) in manifests.all.iter().enumerate() {
        for script in &manifest.scripts {
            for r in script_refs(script) {
                match r {
                    ScriptRef::Binary(bin) => used.binary(i, &bin),
                    ScriptRef::Package(request) => used.run(i, &request),
                }
            }
        }
        for request in tsconfig_refs(&manifest.dir) {
            used.import(i, &request);
        }
    }

    let mut out = Vec::new();
    for (i, manifest) in manifests.all.iter().enumerate() {
        for field in [Field::Dependencies, Field::DevDependencies] {
            for name in manifest.listed.get(&field).into_iter().flatten() {
                let duplicate = field == Field::DevDependencies
                    && manifest.listed.get(&Field::Dependencies).is_some_and(|d| d.contains(name));
                if !duplicate && (used.sets[i].contains(name) || is_always_used(name)) {
                    continue;
                }
                let line = dependency_line(&manifest.text, field.key(), name);
                out.push(Unused {
                    manifest: manifest.dir.join("package.json"),
                    name: name.clone(),
                    field,
                    line,
                });
            }
        }
    }
    debug!("{} unused packages in {} package.json files", out.len(), manifests.all.len());
    out
}

fn is_always_used(name: &str) -> bool {
    ALWAYS_USED.contains(&name) || GLOBAL_TYPES.contains(&name)
}

/// A package.json of the root or a workspace package.
#[derive(Debug)]
struct Manifest {
    dir: PathBuf,
    text: String,
    /// The packages in each field, in the order they are written.
    listed: HashMap<Field, Vec<String>>,
    /// Every package in any field.
    all: HashSet<String>,
    scripts: Vec<String>,
    /// The nearest manifest in an ancestor directory.
    parent: Option<usize>,
}

#[derive(Debug)]
struct Manifests {
    all: Vec<Manifest>,
    by_dir: HashMap<PathBuf, usize>,
}

impl Manifests {
    fn load(project: &Project) -> Self {
        let mut dirs: Vec<PathBuf> = vec![project.root.clone()];
        dirs.extend(project.workspace_packages().into_iter().map(|(_, d)| d.to_path_buf()));
        dirs.sort();
        dirs.dedup();
        let mut all: Vec<Manifest> = dirs
            .into_iter()
            .filter_map(|dir| {
                let text = fs::read_to_string(dir.join("package.json")).ok()?;
                let json: Value = serde_json::from_str(&text).ok()?;
                let mut listed = HashMap::new();
                for field in FIELDS {
                    if let Some(Value::Object(deps)) = json.get(field.key()) {
                        listed.insert(field, deps.keys().cloned().collect::<Vec<_>>());
                    }
                }
                let all = listed.values().flatten().cloned().collect();
                let scripts = match json.get("scripts") {
                    Some(Value::Object(s)) => {
                        s.values().filter_map(Value::as_str).map(str::to_owned).collect()
                    }
                    _ => Vec::new(),
                };
                Some(Manifest { dir, text, listed, all, scripts, parent: None })
            })
            .collect();
        let by_dir: HashMap<PathBuf, usize> =
            all.iter().enumerate().map(|(i, m)| (m.dir.clone(), i)).collect();
        for m in &mut all {
            m.parent = m.dir.ancestors().skip(1).find_map(|a| by_dir.get(a).copied());
        }
        Self { all, by_dir }
    }

    /// The manifest of the nearest directory at or above `dir`.
    fn owner(&self, dir: &Path) -> Option<usize> {
        dir.ancestors().find_map(|a| self.by_dir.get(a).copied())
    }

    /// `start` and the manifests above it, nearest first.
    fn chain(&self, start: usize) -> impl Iterator<Item = usize> + '_ {
        std::iter::successors(Some(start), |&i| self.all[i].parent)
    }
}

/// The packages each manifest uses.
struct Used<'a> {
    manifests: &'a Manifests,
    project: &'a Project,
    sets: Vec<HashSet<String>>,
}

impl<'a> Used<'a> {
    fn new(manifests: &'a Manifests, project: &'a Project) -> Self {
        Self { manifests, project, sets: vec![HashSet::new(); manifests.all.len()] }
    }

    /// Marks `name` used in the nearest manifest from `from` up that lists
    /// it. Returns that manifest.
    fn mark(&mut self, from: usize, name: &str) -> Option<usize> {
        let i = self.manifests.chain(from).find(|&i| self.manifests.all[i].all.contains(name))?;
        self.sets[i].insert(name.to_string());
        Some(i)
    }

    /// A file of manifest `from` imports `request`: the package, its
    /// `@types` package, and the peer dependencies it declares are used.
    fn import(&mut self, from: usize, request: &str) {
        let Some(name) = package_name(request) else { return };
        self.run(from, name);
        if name.starts_with("@types/") {
            return;
        }
        let installed = self.installed(from, name);
        if !installed.as_ref().is_some_and(|p| ships_types(p)) {
            self.mark(from, &types_package(name));
        }
        let peers = installed.as_ref().and_then(|p| p.get("peerDependencies"));
        if let Some(Value::Object(peers)) = peers {
            for peer in peers.keys() {
                self.mark(from, peer);
                if !self.installed(from, peer).is_some_and(|p| ships_types(&p)) {
                    self.mark(from, &types_package(peer));
                }
            }
        }
    }

    /// A script of manifest `from` names `request` as a package to load or
    /// run: the package is used.
    fn run(&mut self, from: usize, request: &str) {
        let Some(name) = package_name(request) else { return };
        self.mark(from, name);
    }

    /// A script of manifest `from` runs `bin`: the nearest listed package
    /// that provides it is used, preferring one known to provide it over
    /// one merely named after it.
    fn binary(&mut self, from: usize, bin: &str) {
        let manifests = self.manifests;
        for i in manifests.chain(from) {
            let best = manifests.all[i]
                .all
                .iter()
                .filter_map(|name| Some((self.provides(i, name, bin)?, name)))
                .min();
            if let Some((_, name)) = best {
                self.sets[i].insert(name.clone());
                return;
            }
        }
    }

    /// Whether package `name`, listed by manifest `i`, has the binary
    /// `bin`: `Some(Provides::Known)` when its installed `bin` field or
    /// [`KNOWN_BINARIES`] says so, `Some(Provides::ByName)` when it isn't
    /// installed and is named after the binary.
    fn provides(&self, i: usize, name: &str, bin: &str) -> Option<Provides> {
        if let Some(pkg) = self.installed(i, name) {
            let known = match pkg.get("bin") {
                Some(Value::String(_)) => unscoped(name) == bin,
                Some(Value::Object(bins)) => bins.contains_key(bin),
                _ => false,
            };
            return known.then_some(Provides::Known);
        }
        if KNOWN_BINARIES.iter().any(|(pkg, bins)| *pkg == name && bins.contains(&bin)) {
            Some(Provides::Known)
        } else if unscoped(name) == bin {
            Some(Provides::ByName)
        } else {
            None
        }
    }

    /// The package.json of `name` as installed for manifest `from`: in the
    /// `node_modules` of its directory or one above it, up to the root.
    fn installed(&self, from: usize, name: &str) -> Option<std::sync::Arc<Value>> {
        let dir = &self.manifests.all[from].dir;
        dir.ancestors()
            .take_while(|a| a.starts_with(&self.project.root))
            .find_map(|a| self.project.package_json(&a.join("node_modules").join(name)))
    }
}

/// How sure it is that a package provides a binary; the surer wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Provides {
    Known,
    ByName,
}

/// The package a bare request names, as `@scope/pkg` for
/// `@scope/pkg/sub`; `None` for paths and `node:` modules.
fn package_name(request: &str) -> Option<&str> {
    if !is_bare(request) {
        return None;
    }
    let name = split_package_request(request).0;
    (!name.starts_with('@') || name.contains('/')).then_some(name)
}

fn unscoped(name: &str) -> &str {
    name.rsplit_once('/').map_or(name, |(_, n)| n)
}

/// The DefinitelyTyped package for `name`: `@types/react` for `react`,
/// `@types/scope__pkg` for `@scope/pkg`.
fn types_package(name: &str) -> String {
    match name.strip_prefix('@').and_then(|n| n.split_once('/')) {
        Some((scope, pkg)) => format!("@types/{scope}__{pkg}"),
        None => format!("@types/{name}"),
    }
}

/// Whether an installed package.json declares its own types.
fn ships_types(pkg: &Value) -> bool {
    fn has_types_condition(v: &Value) -> bool {
        match v {
            Value::Object(o) => o.iter().any(|(k, v)| k == "types" || has_types_condition(v)),
            Value::Array(a) => a.iter().any(has_types_condition),
            _ => false,
        }
    }
    ["types", "typings"]
        .iter()
        .any(|k| pkg.get(*k).and_then(Value::as_str).is_some_and(|s| !s.is_empty()))
        || pkg.get("exports").is_some_and(has_types_condition)
}

/// The packages the tsconfig files in `dir` name: `extends`,
/// `compilerOptions.types` (each also as its `@types` package),
/// `compilerOptions.jsxImportSource` and `compilerOptions.plugins[].name`.
fn tsconfig_refs(dir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<PathBuf> = entries
        .filter_map(std::result::Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                n == "tsconfig.json" || (n.starts_with("tsconfig.") && n.ends_with(".json"))
            })
        })
        .collect();
    files.sort();
    let mut out = Vec::new();
    for file in files {
        let Some(json) = fs::read_to_string(&file).ok().and_then(|s| parse_jsonc(&s)) else {
            continue;
        };
        match json.get("extends") {
            Some(Value::String(s)) => out.push(s.clone()),
            Some(Value::Array(a)) => {
                out.extend(a.iter().filter_map(Value::as_str).map(str::to_owned));
            }
            _ => {}
        }
        let Some(options) = json.get("compilerOptions") else { continue };
        if let Some(Value::Array(types)) = options.get("types") {
            for t in types.iter().filter_map(Value::as_str) {
                out.push(t.to_string());
                if let Some(name) = package_name(t) {
                    out.push(types_package(name));
                }
            }
        }
        if let Some(Value::String(s)) = options.get("jsxImportSource") {
            out.push(s.clone());
        }
        if let Some(Value::Array(plugins)) = options.get("plugins") {
            out.extend(plugins.iter().filter_map(|p| p.get("name")?.as_str()).map(str::to_owned));
        }
    }
    out
}

/// The 1-based line of `"name":` inside the `field` object of a
/// package.json's text, or 0 if it can't be found.
fn dependency_line(text: &str, field: &str, name: &str) -> u32 {
    let Some(start) = find_key(text, field, 0) else { return 0 };
    let end = text[start..].find('}').map_or(text.len(), |i| start + i);
    match find_key(&text[..end], name, start) {
        Some(at) => u32::try_from(text[..at].matches('\n').count() + 1).unwrap_or(0),
        None => 0,
    }
}

/// Byte offset, from `from`, of the quoted `key` followed by a colon.
fn find_key(text: &str, key: &str, from: usize) -> Option<usize> {
    let quoted = format!("\"{key}\"");
    let mut at = from;
    while let Some(i) = text[at..].find(&quoted) {
        let pos = at + i;
        let after = &text[pos + quoted.len()..];
        if after.trim_start().starts_with(':') {
            return Some(pos);
        }
        at = pos + quoted.len();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn types_packages() {
        assert_eq!(types_package("react"), "@types/react");
        assert_eq!(types_package("@eslint/js"), "@types/eslint__js");
    }

    #[test]
    fn package_names() {
        assert_eq!(package_name("@scope/pkg/sub"), Some("@scope/pkg"));
        assert_eq!(package_name("ts-node/register"), Some("ts-node"));
        assert_eq!(package_name("@scope"), None);
        assert_eq!(package_name("node:fs"), None);
        assert_eq!(package_name("./a"), None);
    }

    #[test]
    fn lines_in_package_json() {
        let text = "{\n  \"name\": \"x\",\n  \"dependencies\": {\n    \"a\": \"1\",\n    \"b\": \"1\"\n  },\n  \"devDependencies\": {\n    \"b\": \"1\"\n  }\n}\n";
        assert_eq!(dependency_line(text, "dependencies", "b"), 5);
        assert_eq!(dependency_line(text, "devDependencies", "b"), 8);
        assert_eq!(dependency_line(text, "devDependencies", "a"), 0);
    }

    #[test]
    fn types_conditions() {
        assert!(ships_types(&serde_json::json!({"types": "index.d.ts"})));
        assert!(ships_types(&serde_json::json!({"exports": {".": {"types": "./a.d.ts"}}})));
        assert!(!ships_types(&serde_json::json!({"main": "index.js", "types": ""})));
    }
}
