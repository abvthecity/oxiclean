use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use globset::GlobSet;
use oxiclean_core::{
    Analysis, BuildOptions, CheckResult, DependencyField, Finding, FindingKind, Project, Result,
    RulePolicy, UnusedDependency, split_package_request,
};
use serde_json::Value;
use tracing::{debug, info, warn};

use crate::{
    config::{Config, DEFAULT_THRESHOLD, DEPENDENCIES_RULE},
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
enum Field {
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
    fn key(self) -> &'static str {
        match self {
            Self::Dependencies => "dependencies",
            Self::DevDependencies => "devDependencies",
            Self::Peer => "peerDependencies",
            Self::Optional => "optionalDependencies",
        }
    }
}

/// The key of a reported field in package.json.
fn field_key(field: DependencyField) -> &'static str {
    match field {
        DependencyField::Dependencies => Field::Dependencies.key(),
        DependencyField::DevDependencies => Field::DevDependencies.key(),
    }
}

/// Runs the unused dependencies check described by `cfg`.
///
/// # Errors
/// Fails if the project root can't be found, the config file is invalid, or
/// no entry files are found.
pub fn run_dependencies_check(cfg: &Config) -> Result<CheckResult> {
    info!("Starting {DEPENDENCIES_RULE} check");
    // Type-only imports use packages too, so they are resolved and the
    // files only they reach are read.
    let analysis =
        cfg.common.analyze_package_entries(BuildOptions::default().resolve_passive(true))?;
    let policy = analysis.policy(DEPENDENCIES_RULE, DEFAULT_THRESHOLD)?;
    let ignored = analysis.config.ignored_dependencies()?;
    Ok(check(&analysis, &policy, &ignored))
}

/// Reports each package of the root's or a workspace package's
/// package.json that nothing uses (see [`unused_dependencies`]), in the
/// fields the rule's `fields` option picks for that package.json. Packages
/// `ignored` matches are left out.
#[must_use]
pub fn check(analysis: &Analysis, policy: &RulePolicy, ignored: &GlobSet) -> CheckResult {
    if policy.min_threshold().is_none() {
        info!("{DEPENDENCIES_RULE} is disabled");
        return analysis.finish(policy, Vec::new());
    }
    let findings: Vec<Finding> = unused_dependencies(analysis)
        .into_iter()
        .filter(|u| {
            let rel = u.manifest.strip_prefix(&analysis.root).unwrap_or(&u.manifest);
            let package = analysis.project.package_for(&u.manifest);
            policy.resolve(rel, package).fields.includes(u.field) && !ignored.is_match(&u.name)
        })
        .map(|u| {
            let dep = UnusedDependency::new(u.name, field_key(u.field).to_string(), u.line);
            Finding::new(u.manifest, FindingKind::UnusedDependency(dep), 1)
        })
        .collect();
    info!("{DEPENDENCIES_RULE} check complete. Found {} unused", findings.len());
    analysis.finish(policy, findings)
}

/// A package a package.json lists that nothing uses.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Unused {
    /// The package.json.
    pub manifest: PathBuf,
    pub name: String,
    pub field: DependencyField,
    /// 1-based line of the package in the package.json, or 0 when unknown.
    pub line: u32,
}

/// The packages in `dependencies` and `devDependencies` of the root's and
/// each workspace package's package.json that nothing uses, by package.json
/// and line, as knip's `dependencies` and `devDependencies` find them with
/// its plugins off. A package is used when:
///
/// - a file the package entries reach imports it, in any form: `import`,
///   `import type`, `require`, `import()`, `typeof import()` or
///   `/// <reference types>`. The import counts for the
///   nearest package.json that lists the package, from the file's own
///   package up to the root, as Node's resolution finds it;
/// - a script in the package.json runs one of its binaries, through `npx`,
///   `pnpm exec` or a wrapper such as `cross-env` too (see `script_refs`),
///   or loads it with `--require` or `--import`. Binaries are matched to
///   packages by the installed packages' `bin` field, or else by
///   a short list of known binaries or the unscoped name; every package of the nearest
///   package.json that provides the binary is used;
/// - a config file beside the package.json names it, as a plugin reads it:
///   tsconfig `extends`, `compilerOptions.types`, `jsxImportSource` or
///   `plugins`;
/// - an installed package that is imported, loaded by a script or named by
///   a tsconfig lists it in `peerDependencies`;
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

    // Each (manifest, package) and (manifest, binary) is looked up once,
    // however many files import the package or scripts run the binary.
    let mut imports: HashSet<(usize, &str)> = HashSet::new();
    let mut owners: HashMap<&Path, Option<usize>> = HashMap::new();
    for module in analysis.graph.modules() {
        if module.in_node_modules() || module.packages.is_empty() {
            continue;
        }
        let Some(dir) = module.path.parent() else { continue };
        let owner = *owners.entry(dir).or_insert_with(|| manifests.owner(dir));
        let Some(owner) = owner else { continue };
        imports.extend(module.packages.iter().filter_map(|r| Some((owner, package_name(r)?))));
    }
    let mut script_imports: HashSet<(usize, String)> = HashSet::new();
    let mut binaries: HashSet<(usize, String)> = HashSet::new();
    for (i, manifest) in manifests.all.iter().enumerate() {
        let is_script = |name: &str| manifest.script_names.contains(name);
        let tool_scripts = analysis.plugins.scripts(&manifest.dir);
        for script in manifest.scripts.iter().chain(&tool_scripts) {
            for r in script_refs(script, &is_script) {
                match r {
                    ScriptRef::Binary(bin) => binaries.insert((i, bin)),
                    ScriptRef::Package(request) => script_imports.insert((i, request)),
                };
            }
        }
        script_imports
            .extend(analysis.plugins.references(&manifest.dir).into_iter().map(|r| (i, r)));
    }
    for (i, name) in imports {
        used.import(i, name);
    }
    for (i, request) in &script_imports {
        if let Some(name) = package_name(request) {
            used.import(*i, name);
        }
    }
    for (i, bin) in &binaries {
        used.binary(*i, bin);
    }

    let mut out = Vec::new();
    for (i, manifest) in manifests.all.iter().enumerate() {
        for (field, reported) in [
            (Field::Dependencies, DependencyField::Dependencies),
            (Field::DevDependencies, DependencyField::DevDependencies),
        ] {
            for name in manifest.listed.get(&field).into_iter().flatten() {
                let duplicate =
                    field == Field::DevDependencies && manifest.dependencies.contains(name);
                if !duplicate && (used.sets[i].contains(name) || is_always_used(name)) {
                    continue;
                }
                let line = dependency_line(&manifest.text, field.key(), name);
                out.push(Unused {
                    manifest: manifest.dir.join("package.json"),
                    name: name.clone(),
                    field: reported,
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
    /// The packages in each field, sorted.
    listed: HashMap<Field, Vec<String>>,
    /// The packages in `dependencies`.
    dependencies: HashSet<String>,
    /// Every package in any field.
    all: HashSet<String>,
    scripts: Vec<String>,
    script_names: HashSet<String>,
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
                let path = dir.join("package.json");
                let text = fs::read_to_string(&path).ok()?;
                let json: Value = match serde_json::from_str(text.trim_start_matches('\u{feff}')) {
                    Ok(json) => json,
                    Err(e) => {
                        warn!("Skipping {}: {e}", path.display());
                        return None;
                    }
                };
                let mut listed = HashMap::new();
                for field in FIELDS {
                    if let Some(Value::Object(deps)) = json.get(field.key()) {
                        listed.insert(field, deps.keys().cloned().collect::<Vec<_>>());
                    }
                }
                let all = listed.values().flatten().cloned().collect();
                let dependencies =
                    listed.get(&Field::Dependencies).into_iter().flatten().cloned().collect();
                let (scripts, script_names) = match json.get("scripts") {
                    Some(Value::Object(s)) => (
                        s.values().filter_map(Value::as_str).map(str::to_owned).collect(),
                        s.keys().cloned().collect(),
                    ),
                    _ => (Vec::new(), HashSet::new()),
                };
                Some(Manifest {
                    dir,
                    text,
                    listed,
                    dependencies,
                    all,
                    scripts,
                    script_names,
                    parent: None,
                })
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
    /// The installed package.json of each package, by the manifest it is
    /// looked up for.
    installed: HashMap<(usize, String), Option<Arc<Value>>>,
    /// The packages each manifest lists, by the binaries they provide,
    /// built when a script of the manifest's runs a binary.
    binaries: HashMap<usize, HashMap<String, Vec<String>>>,
}

impl<'a> Used<'a> {
    fn new(manifests: &'a Manifests, project: &'a Project) -> Self {
        Self {
            manifests,
            project,
            sets: vec![HashSet::new(); manifests.all.len()],
            installed: HashMap::new(),
            binaries: HashMap::new(),
        }
    }

    /// Marks `name` used in the nearest manifest from `from` up that lists
    /// it.
    fn mark(&mut self, from: usize, name: &str) {
        if let Some(i) =
            self.manifests.chain(from).find(|&i| self.manifests.all[i].all.contains(name))
        {
            self.sets[i].insert(name.to_string());
        }
    }

    /// Manifest `from` imports, loads or names package `name`: the package,
    /// its `@types` package, and the peer dependencies it declares are used.
    fn import(&mut self, from: usize, name: &str) {
        self.mark(from, name);
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

    /// A script of manifest `from` runs `bin`: in the nearest manifest from
    /// `from` up that lists a package providing it, every such package is
    /// used, as `playwright` and `@playwright/test` both are.
    fn binary(&mut self, from: usize, bin: &str) {
        let manifests = self.manifests;
        for i in manifests.chain(from) {
            let Some(providers) = self.providers(i).get(bin).cloned() else { continue };
            self.sets[i].extend(providers);
            return;
        }
    }

    /// The packages manifest `i` lists, by each binary they provide: the
    /// installed package's `bin` field, else [`KNOWN_BINARIES`] and, for an
    /// unscoped package, its name.
    fn providers(&mut self, i: usize) -> &HashMap<String, Vec<String>> {
        if !self.binaries.contains_key(&i) {
            let mut names: Vec<&String> = self.manifests.all[i].all.iter().collect();
            names.sort();
            let mut map: HashMap<String, Vec<String>> = HashMap::new();
            for name in names {
                let mut add =
                    |bin: &str| map.entry(bin.to_string()).or_default().push(name.clone());
                if let Some(pkg) = self.installed(i, name) {
                    match pkg.get("bin") {
                        Some(Value::String(_)) => add(unscoped(name)),
                        Some(Value::Object(bins)) => bins.keys().for_each(|b| add(b)),
                        _ => {}
                    }
                    continue;
                }
                let known = KNOWN_BINARIES.iter().filter(|(pkg, _)| pkg == name);
                known.flat_map(|(_, bins)| bins.iter()).for_each(|b| add(b));
                // A scoped package isn't named after its binary: `@types/jest`
                // doesn't provide `jest`.
                if !name.starts_with('@') {
                    add(name);
                }
            }
            self.binaries.insert(i, map);
        }
        &self.binaries[&i]
    }

    /// The package.json of `name` as installed for manifest `from`: in the
    /// `node_modules` of its directory or one above it, up to the root.
    fn installed(&mut self, from: usize, name: &str) -> Option<Arc<Value>> {
        let key = (from, name.to_string());
        if let Some(found) = self.installed.get(&key) {
            return found.clone();
        }
        let dir = &self.manifests.all[from].dir;
        let found = dir
            .ancestors()
            .take_while(|a| a.starts_with(&self.project.root))
            .find_map(|a| self.project.package_json(&a.join("node_modules").join(name)));
        self.installed.insert(key, found.clone());
        found
    }
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

/// The 1-based line of the key `name` in the top-level `field` object of a
/// package.json's text, or 0 if it can't be found.
fn dependency_line(text: &str, field: &str, name: &str) -> u32 {
    let bytes = text.as_bytes();
    let (mut depth, mut line, mut i) = (0_usize, 1_u32, 0);
    // The key at depth 1 whose value is being read.
    let mut top_key = "";
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => line += 1,
            b'{' | b'[' => depth += 1,
            b'}' | b']' => depth = depth.saturating_sub(1),
            b'"' => {
                let start = i + 1;
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
                let Some(string) = text.get(start..i.min(text.len())) else { return 0 };
                let is_key = text.get(i + 1..).is_some_and(|r| r.trim_start().starts_with(':'));
                if is_key && depth == 1 {
                    top_key = string;
                } else if is_key && depth == 2 && top_key == field && string == name {
                    return line;
                }
            }
            _ => {}
        }
        i += 1;
    }
    0
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
        // A nested `dependencies` key earlier in the file is not the field.
        let text = "{\n  \"pnpm\": { \"packageExtensions\": { \"x\": { \"dependencies\": { \"a\": \"1\" } } } },\n  \"dependencies\": {\n    \"a\": \"1\"\n  }\n}\n";
        assert_eq!(dependency_line(text, "dependencies", "a"), 4);
    }

    #[test]
    fn types_conditions() {
        assert!(ships_types(&serde_json::json!({"types": "index.d.ts"})));
        assert!(ships_types(&serde_json::json!({"exports": {".": {"types": "./a.d.ts"}}})));
        assert!(!ships_types(&serde_json::json!({"main": "index.js", "types": ""})));
    }
}
