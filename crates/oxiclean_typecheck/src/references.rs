//! `references`: project references that don't match what packages import.
//!
//! With project references, `tsc -b` checks each package once and its
//! consumers read the `.d.ts` it emits. Without them, or with them out of
//! date, every program that imports a sibling package's source checks that
//! source again. Reported, as `code`:
//!
//! - `missing-reference`: a program that takes part in a build with
//!   references imports the source of a workspace package with a
//!   `composite` config, without referencing it. Imports that resolve to
//!   built `.d.ts` files cost nothing and aren't reported.
//! - `stale-reference`: it references a workspace package it never imports.
//! - `not-composite`: a config without `composite` that a program with
//!   files references, which `tsc -b` rejects.
//! - `missing-target`: a reference to a config that doesn't exist.
//! - `reference-cycle`: configs that reference each other, which `tsc -b`
//!   can't order.
//! - `source-imports`: a package whose source two or more other packages
//!   type-check without referencing it, through `paths` or a package.json
//!   `types` pointing at `.ts`.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
};

use dashmap::DashMap;
use oxiclean_core::{
    CheckResult, Finding, FindingKind, Issue, Project, Related, Result,
    fix::{Fix, FixOp},
    jsonc_tree::Segment,
    parse_imports, resolve, sequence, split_package_request,
};
use rayon::prelude::*;
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::{
    config::{Config, DEFAULT_THRESHOLD, REFERENCES_RULE},
    tsconfig::{TsConfigFile, TsConfigs, relative, slashes},
};

/// Runs the `references` check with `cfg`.
///
/// # Errors
/// Fails if the root can't be found or read, or the config file is invalid.
pub fn run_references_check(cfg: &Config) -> Result<CheckResult> {
    info!("Starting {REFERENCES_RULE} check");
    let workspace = cfg.common.workspace()?;
    let policy = workspace.policy(REFERENCES_RULE, DEFAULT_THRESHOLD)?;
    let configs = TsConfigs::load(&workspace.project);
    let findings = check(&workspace.project, &configs);
    info!("{REFERENCES_RULE} check complete. Found {} findings", findings.len());
    Ok(workspace.finish(&policy, findings, configs.configs.len()))
}

/// Where a program imports a workspace package first, and whether the
/// import reaches the package's source.
#[derive(Debug)]
struct ImportSite {
    file: PathBuf,
    line: u32,
    source: bool,
}

/// The findings for `configs` in `project`.
#[must_use]
pub(crate) fn check(project: &Project, configs: &TsConfigs) -> Vec<Finding> {
    let imports = package_imports(project, configs);
    let closure = reference_closure(configs);
    let uses_references = configs
        .configs
        .iter()
        .any(|c| !c.references.is_empty() || c.flag("composite") == Some(true));
    let mut findings = Vec::new();
    let mut missing = BTreeSet::new();
    if uses_references {
        let (mismatches, reported) = reference_mismatches(project, configs, &imports, &closure);
        findings.extend(mismatches);
        missing = reported;
        findings.extend(reference_targets(configs));
        findings.extend(reference_cycles(configs));
    }
    findings.extend(source_imports(project, configs, &imports, &closure, &missing));
    findings
}

/// What one file imports: workspace packages, with the line of each,
/// and the local files it reaches.
#[derive(Debug, Default)]
struct FileImports {
    packages: Vec<(String, u32)>,
    local: Vec<PathBuf>,
}

/// The imports of `file`.
fn read_imports(
    project: &Project,
    cache: &DashMap<(PathBuf, String), Option<PathBuf>>,
    file: &Path,
) -> FileImports {
    let specs = match parse_imports(file) {
        Ok(parsed) => parsed.specs,
        Err(e) => {
            warn!("Skipping {}: {e}", file.display());
            return FileImports::default();
        }
    };
    let mut out = FileImports::default();
    for s in specs {
        if s.request.starts_with('.') || s.request.starts_with('/') {
            let local = resolve(project, file, &s.request, cache).ok().flatten();
            out.local
                .extend(local.filter(|l| !l.components().any(|c| c.as_os_str() == "node_modules")));
        } else {
            let (name, _) = split_package_request(&s.request);
            if project.workspace_package(name).is_some() {
                out.packages.push((name.to_string(), s.line));
            }
        }
    }
    out
}

/// For each program, the workspace packages it imports, other than its
/// own, with where each is first imported. Like `tsc`, this follows local
/// imports from the root files, so a program limited by `files` and the
/// `.vue` or `.svelte` files its code imports count, and so do the
/// packages `compilerOptions.types` loads.
fn package_imports(
    project: &Project,
    configs: &TsConfigs,
) -> HashMap<PathBuf, BTreeMap<String, ImportSite>> {
    let cache = DashMap::new();
    let mut parsed: HashMap<PathBuf, FileImports> = HashMap::new();
    let mut frontier: BTreeSet<PathBuf> =
        configs.programs().flat_map(|p| p.root_files.iter().cloned()).collect();
    while !frontier.is_empty() {
        let read: Vec<(PathBuf, FileImports)> = frontier
            .into_par_iter()
            .map(|f| {
                let imports = read_imports(project, &cache, &f);
                (f, imports)
            })
            .collect();
        parsed.extend(read);
        frontier = parsed
            .values()
            .flat_map(|i| i.local.iter())
            .filter(|l| !parsed.contains_key(*l))
            .cloned()
            .collect();
    }
    configs
        .programs()
        .map(|p| {
            let own = project.package_for(&p.path);
            let mut map: BTreeMap<String, ImportSite> = BTreeMap::new();
            let mut add = |name: &str, file: &Path, line: u32| {
                if Some(name) != own && !map.contains_key(name) {
                    let source = project
                        .workspace_package(name)
                        .is_some_and(|dir| reaches_source(project, p, name, dir));
                    let site = ImportSite { file: file.to_path_buf(), line, source };
                    map.insert(name.to_string(), site);
                }
            };
            if let Some(Value::Array(types)) = p.options.get("types") {
                let line = p.line_of(&["compilerOptions", "types"]);
                for t in types.iter().filter_map(Value::as_str) {
                    if project.workspace_package(t).is_some() {
                        add(t, &p.path, line);
                    }
                }
            }
            let mut seen: HashSet<&Path> = p.root_files.iter().map(PathBuf::as_path).collect();
            let mut queue: VecDeque<&Path> = p.root_files.iter().map(PathBuf::as_path).collect();
            while let Some(f) = queue.pop_front() {
                let Some(imports) = parsed.get(f) else { continue };
                for (name, line) in &imports.packages {
                    add(name, f, *line);
                }
                for l in &imports.local {
                    if seen.insert(l) {
                        queue.push_back(l);
                    }
                }
            }
            (p.path.clone(), map)
        })
        .collect()
}

/// The configs each config references, directly or through the configs
/// it references: `tsc -b` builds them all first, and imports of any of
/// them read its `.d.ts`.
fn reference_closure(configs: &TsConfigs) -> HashMap<&Path, BTreeSet<&Path>> {
    let direct: HashMap<&Path, Vec<&Path>> = configs
        .configs
        .iter()
        .map(|c| (c.path.as_path(), c.references.iter().map(|r| r.target.as_path()).collect()))
        .collect();
    direct
        .keys()
        .map(|&start| {
            let mut seen: BTreeSet<&Path> = BTreeSet::new();
            let mut stack: Vec<&Path> = direct.get(start).cloned().unwrap_or_default();
            while let Some(t) = stack.pop() {
                if seen.insert(t) {
                    stack.extend(direct.get(t).into_iter().flatten().copied());
                }
            }
            (start, seen)
        })
        .collect()
}

/// Whether one of `targets` is a config of the workspace package `name`.
fn covers(project: &Project, targets: &BTreeSet<&Path>, name: &str) -> bool {
    targets.iter().any(|t| project.package_for(t) == Some(name))
}

/// Whether `program` takes part in a build with references: it has some,
/// something references it, or it is `composite`.
fn in_build(program: &TsConfigFile, referenced: &BTreeSet<&Path>) -> bool {
    !program.references.is_empty()
        || referenced.contains(program.path.as_path())
        || program.flag("composite") == Some(true)
}

/// Missing and stale references of the programs that build with them,
/// with the (program, package) pairs reported missing.
fn reference_mismatches<'a>(
    project: &Project,
    configs: &'a TsConfigs,
    imports: &'a HashMap<PathBuf, BTreeMap<String, ImportSite>>,
    closure: &HashMap<&Path, BTreeSet<&Path>>,
) -> (Vec<Finding>, BTreeSet<(&'a Path, &'a str)>) {
    let mut missing = BTreeSet::new();
    let referenced: BTreeSet<&Path> = configs
        .configs
        .iter()
        .flat_map(|c| c.references.iter().map(|r| r.target.as_path()))
        .collect();
    let mut findings = Vec::new();
    for program in configs.programs().filter(|p| !p.root_files.is_empty()) {
        if !in_build(program, &referenced) {
            continue;
        }
        let Some(imported) = imports.get(&program.path) else { continue };
        let own = project.package_for(&program.path);
        let empty = BTreeSet::new();
        let reached = closure.get(program.path.as_path()).unwrap_or(&empty);
        for (name, site) in imported {
            let Some(dir) = project.workspace_package(name) else { continue };
            // An import that resolves to built `.d.ts` files costs nothing
            // to check, whatever builds them; one that reaches source needs
            // a reference to read the `.d.ts` instead.
            let Some(target) = reference_target(configs, dir) else { continue };
            if !site.source || covers(project, reached, name) {
                continue;
            }
            missing.insert((program.path.as_path(), name.as_str()));
            let path = if target.name() == "tsconfig.json" {
                relative(&program.dir, dir)
            } else {
                relative(&program.dir, &target.path)
            };
            let message = format!(
                "imports {name} but doesn't reference its tsconfig, so `tsc -b` checks it again instead of reading its .d.ts"
            );
            let fix = Fix::new(
                program.path.clone(),
                format!("add {{ \"path\": \"{path}\" }} to \"references\""),
                vec![FixOp::JsonAppend {
                    path: vec![Segment::from("references")],
                    value: json!({ "path": path }),
                    seed: Vec::new(),
                }],
            );
            let issue = Issue::new(
                "missing-reference",
                name.clone(),
                message,
                program.line_of(&["references"]),
            )
            .with_related(vec![Related::new(
                site.file.clone(),
                site.line,
                format!("imports {name}"),
            )])
            .with_fix(fix);
            findings.push(Finding::new(program.path.clone(), FindingKind::Issue(issue), 1));
        }
        // Files of formats `tsc` doesn't list, such as `.vue`, may import
        // what the root files don't.
        let other_formats = program
            .include
            .iter()
            .flatten()
            .any(|i| [".vue", ".svelte", ".astro", ".mdx"].iter().any(|e| i.contains(e)));
        for r in program.references.iter().filter(|_| !other_formats) {
            // Only references to other workspace packages: a solution
            // config's references to its own variants are structure.
            let Some(name) = project.package_for(&r.target) else { continue };
            // A reference is needed when the program imports the package
            // or one the referenced config references in turn.
            let mut through: BTreeSet<&Path> =
                closure.get(r.target.as_path()).cloned().unwrap_or_default();
            through.insert(&r.target);
            if Some(name) == own || imported.keys().any(|i| covers(project, &through, i)) {
                continue;
            }
            let message = format!(
                "references {name}, which none of its files import, so `tsc -b` builds it first for nothing"
            );
            let fix = Fix::new(
                program.path.clone(),
                format!("remove {{ \"path\": \"{}\" }} from \"references\"", r.raw),
                vec![FixOp::JsonRemove {
                    path: vec![Segment::from("references")],
                    value: r.value.clone(),
                }],
            );
            let issue =
                Issue::new("stale-reference", name.to_string(), message, r.line).with_fix(fix);
            findings.push(Finding::new(program.path.clone(), FindingKind::Issue(issue), 1));
        }
    }
    (findings, missing)
}

/// The config a reference to the package in `dir` should point at: a
/// `composite` config with files, `tsconfig.json` first. A package with
/// none can't be referenced.
fn reference_target<'a>(configs: &'a TsConfigs, dir: &Path) -> Option<&'a TsConfigFile> {
    let mut buildable = configs
        .configs
        .iter()
        .filter(|c| c.dir == dir && c.flag("composite") == Some(true) && !c.root_files.is_empty());
    buildable.clone().find(|c| c.name() == "tsconfig.json").or_else(|| buildable.next())
}

/// References to configs that don't exist, and referenced configs that
/// aren't `composite`.
fn reference_targets(configs: &TsConfigs) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut not_composite: BTreeSet<&Path> = BTreeSet::new();
    for c in &configs.configs {
        for r in &c.references {
            match configs.get(&r.target) {
                // Only configs named `tsconfig*` or `jsconfig*` are loaded.
                None if r.target.is_file() => {}
                None => {
                    let message = format!("references {}, which doesn't exist", r.raw);
                    let issue = Issue::new("missing-target", r.raw.clone(), message, r.line);
                    findings.push(Finding::new(c.path.clone(), FindingKind::Issue(issue), 1));
                }
                // A solution config with no files of its own, such as Vite's
                // `tsconfig.json`, may reference configs that aren't
                // composite; any other config may not, even one with no
                // files.
                Some(t)
                    if t.flag("composite") != Some(true)
                        && !c.root_files.is_empty()
                        && not_composite.insert(&t.path) =>
                {
                    let message = format!(
                        "is referenced by {} but isn't composite, so `tsc -b` refuses to build it",
                        relative(&t.dir, &c.path)
                    );
                    let line = t.line_of(&["compilerOptions"]);
                    let issue = Issue::new("not-composite", "composite", message, line)
                        .with_related(vec![Related::new(c.path.clone(), r.line, "references it")])
                        .with_fix(Fix::json_set(
                            t.path.clone(),
                            &["compilerOptions", "composite"],
                            json!(true),
                        ));
                    findings.push(Finding::new(t.path.clone(), FindingKind::Issue(issue), 1));
                }
                Some(_) => {}
            }
        }
    }
    findings
}

/// Configs that reference each other.
fn reference_cycles(configs: &TsConfigs) -> Vec<Finding> {
    let index: HashMap<&Path, usize> =
        configs.configs.iter().enumerate().map(|(i, c)| (c.path.as_path(), i)).collect();
    let deps: Vec<Vec<usize>> = configs
        .configs
        .iter()
        .map(|c| {
            c.references.iter().filter_map(|r| index.get(r.target.as_path()).copied()).collect()
        })
        .collect();
    let all: Vec<usize> = (0..configs.configs.len()).collect();
    sequence(&deps, &all)
        .cycles
        .into_iter()
        .filter_map(|cycle| {
            let members: Vec<&TsConfigFile> = cycle.iter().map(|&i| &configs.configs[i]).collect();
            let [first, rest @ ..] = members.as_slice() else { return None };
            let mut names: Vec<String> =
                members.iter().map(|m| relative(&first.dir, &m.path)).collect();
            names.push(relative(&first.dir, &first.path));
            let message = format!(
                "references form a cycle that `tsc -b` can't order: {}",
                names.join(" -> ")
            );
            let line = first.line_of(&["references"]);
            let related = rest
                .iter()
                .map(|m| {
                    Related::new(m.path.clone(), m.line_of(&["references"]), "part of the cycle")
                })
                .collect();
            let mut key: Vec<String> = members.iter().map(|m| slashes(&m.path)).collect();
            key.sort();
            let issue =
                Issue::new("reference-cycle", key.join(","), message, line).with_related(related);
            Some(Finding::new(first.path.clone(), FindingKind::Issue(issue), members.len()))
        })
        .collect()
}

/// Packages whose source two or more other packages type-check.
fn source_imports(
    project: &Project,
    configs: &TsConfigs,
    imports: &HashMap<PathBuf, BTreeMap<String, ImportSite>>,
    closure: &HashMap<&Path, BTreeSet<&Path>>,
    missing: &BTreeSet<(&Path, &str)>,
) -> Vec<Finding> {
    // Package name -> its directory and the consumer programs that reach
    // its source, other than those that reference it or already have a
    // missing reference reported. The value counts these configs.
    let mut consumers: BTreeMap<&str, (&Path, BTreeMap<&str, &TsConfigFile>)> = BTreeMap::new();
    for program in configs.programs() {
        let Some(imported) = imports.get(&program.path) else { continue };
        let consumer = project.package_for(&program.path).unwrap_or(".");
        let reached = closure.get(program.path.as_path());
        for (name, site) in imported {
            let Some(dir) = project.workspace_package(name) else { continue };
            let referenced = reached.is_some_and(|r| covers(project, r, name));
            if site.source
                && !referenced
                && !missing.contains(&(program.path.as_path(), name.as_str()))
            {
                consumers
                    .entry(name.as_str())
                    .or_insert_with(|| (dir, BTreeMap::new()))
                    .1
                    .entry(consumer)
                    .or_insert(program);
            }
        }
    }
    consumers
        .into_iter()
        .filter(|(_, (_, by))| by.len() >= 2)
        .map(|(name, (dir, by))| {
            let file = configs
                .configs
                .iter()
                .find(|c| c.dir == dir && c.name() == "tsconfig.json")
                .map_or_else(|| dir.join("package.json"), |c| c.path.clone());
            let list: Vec<&str> = by.keys().copied().collect();
            let message = format!(
                "{name}'s source is type-checked again by each of {} packages that import it ({}): make it composite and reference it, or point its types at built .d.ts files",
                by.len(),
                list.join(", ")
            );
            let related = by
                .values()
                .map(|p| Related::new(p.path.clone(), 0, format!("type-checks {name}'s source")))
                .collect();
            let issue = Issue::new("source-imports", name, message, 0).with_related(related);
            Finding::new(file, FindingKind::Issue(issue), by.len())
        })
        .collect()
}

/// Whether `program` type-checks the source of the workspace package
/// `name` in `dir` when it imports it, rather than built `.d.ts` files.
/// As in `tsc`, `paths` come first, then the package's own entry points.
fn reaches_source(project: &Project, program: &TsConfigFile, name: &str, dir: &Path) -> bool {
    let found =
        paths_target(project, program, name).or_else(|| package_entry(project, program, dir));
    found.is_some_and(|f| f.starts_with(dir) && is_source(&f))
}

/// The file `program`'s `paths` map `name` to: the targets of the exact
/// key, else of the wildcard key with the longest prefix, in order. None
/// when no key matches or no target exists, and `tsc` looks the package up
/// as usual.
fn paths_target(project: &Project, program: &TsConfigFile, name: &str) -> Option<PathBuf> {
    let Some(Value::Object(paths)) = program.options.get("paths") else { return None };
    let (star, targets) = paths
        .iter()
        .filter_map(|(key, targets)| match key.split_once('*') {
            None => (key == name).then_some((usize::MAX, "", targets)),
            Some((prefix, suffix)) => name
                .strip_prefix(prefix)
                .and_then(|rest| rest.strip_suffix(suffix))
                .map(|star| (prefix.len(), star, targets)),
        })
        .max_by_key(|(rank, _, _)| *rank)
        .map(|(_, star, targets)| (star, targets))?;
    targets
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .find_map(|t| resolve_typed(project, Path::new(&t.replacen('*', star, 1))))
}

/// The file `tsc` reads for the package in `dir`: its `exports` `.`
/// entry when the program's module resolution reads `exports`, else
/// `types` or `typings`, else `main`, else `index`. `module` is never read.
fn package_entry(project: &Project, program: &TsConfigFile, dir: &Path) -> Option<PathBuf> {
    let Some(pkg) = project.package_json(dir) else {
        return resolve_typed(project, &dir.join("index"));
    };
    if let Some(exports) = pkg.get("exports").filter(|_| reads_exports(program)) {
        let root = match exports {
            Value::Object(map) if map.keys().any(|k| k.starts_with('.')) => map.get(".")?,
            other => other,
        };
        let target = condition_target(root, &conditions(program))?;
        return resolve_typed(project, &dir.join(target));
    }
    ["types", "typings", "main"]
        .iter()
        .find_map(|k| pkg.get(k)?.as_str())
        .and_then(|t| resolve_typed(project, &dir.join(t)))
        .or_else(|| resolve_typed(project, &dir.join("index")))
}

/// Whether `program`'s module resolution reads package.json `exports`:
/// `node16`, `nodenext` and `bundler` do, `node10` and `classic` don't.
fn reads_exports(program: &TsConfigFile) -> bool {
    let option = |k: &str| program.options.get(k).and_then(Value::as_str).map(str::to_lowercase);
    match option("moduleResolution").as_deref() {
        Some("node16" | "nodenext" | "bundler") => true,
        Some(_) => false,
        None => matches!(
            option("module").as_deref(),
            Some("node16" | "node18" | "node20" | "nodenext" | "preserve")
        ),
    }
}

/// The `exports` conditions `tsc` matches for `program`.
fn conditions(program: &TsConfigFile) -> Vec<String> {
    let mut out: Vec<String> =
        ["types", "import", "require", "node", "default"].iter().map(ToString::to_string).collect();
    if let Some(Value::Array(custom)) = program.options.get("customConditions") {
        out.extend(custom.iter().filter_map(Value::as_str).map(str::to_string));
    }
    out
}

/// The target of an `exports` entry: the first condition in key order
/// that `conditions` has (`types@<version>` counting as `types`).
fn condition_target<'a>(entry: &'a Value, conditions: &[String]) -> Option<&'a str> {
    match entry {
        Value::String(s) => Some(s),
        Value::Array(items) => items.iter().find_map(|i| condition_target(i, conditions)),
        Value::Object(map) => map
            .iter()
            .filter(|(k, _)| {
                let k = k.split('@').next().unwrap_or(k);
                conditions.iter().any(|c| c == k)
            })
            .find_map(|(_, v)| condition_target(v, conditions)),
        _ => None,
    }
}

/// The file `tsc` loads for `path`: `.js` swapped for `.ts`, `.tsx` or
/// `.d.ts`; the file itself; `path` plus one of those extensions; or, for a
/// directory, its package.json entry or `index`.
fn resolve_typed(project: &Project, path: &Path) -> Option<PathBuf> {
    let text = path.to_string_lossy();
    let swapped = [(".mjs", [".mts", ".d.mts"]), (".cjs", [".cts", ".d.cts"])]
        .into_iter()
        .chain([".js", ".jsx"].map(|e| (e, [".ts", ".d.ts"])))
        .find_map(|(js, ts)| {
            let stem = text.strip_suffix(js)?;
            let tsx = (js == ".jsx" || js == ".js").then(|| format!("{stem}.tsx"));
            ts.iter()
                .map(|e| format!("{stem}{e}"))
                .chain(tsx)
                .map(PathBuf::from)
                .find(|c| c.is_file())
        });
    if swapped.is_some() {
        return swapped;
    }
    if path.is_file() {
        return Some(path.to_path_buf());
    }
    if let Some(found) = [".ts", ".tsx", ".d.ts"]
        .iter()
        .map(|e| PathBuf::from(format!("{text}{e}")))
        .find(|c| c.is_file())
    {
        return Some(found);
    }
    if !path.is_dir() {
        return None;
    }
    let pkg = project.package_json(path);
    pkg.as_ref()
        .and_then(|p| ["types", "typings", "main"].iter().find_map(|k| p.get(k)?.as_str()))
        .map(|t| path.join(t))
        .filter(|t| t.as_path() != path)
        .and_then(|t| resolve_file_only(&t))
        .or_else(|| resolve_file_only(&path.join("index")))
}

/// [`resolve_typed`] for a file only, so directories don't recurse.
fn resolve_file_only(path: &Path) -> Option<PathBuf> {
    if path.is_file() {
        return Some(path.to_path_buf());
    }
    let text = path.to_string_lossy();
    let stem =
        [".js", ".mjs", ".cjs", ".jsx"].iter().find_map(|e| text.strip_suffix(e)).unwrap_or(&text);
    [".ts", ".tsx", ".d.ts"]
        .iter()
        .map(|e| PathBuf::from(format!("{stem}{e}")))
        .find(|c| c.is_file())
}

/// Whether `file` is TypeScript source rather than a declaration file.
fn is_source(file: &Path) -> bool {
    let name = file.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
    [".ts", ".tsx", ".mts", ".cts"].iter().any(|e| name.ends_with(e)) && !name.contains(".d.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write(root: &Path, path: &str, text: &str) {
        let p = root.join(path);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    fn codes(root: &Path) -> Vec<String> {
        let project = Project::load(root);
        let configs = TsConfigs::load(&project);
        let mut out: Vec<String> = check(&project, &configs)
            .iter()
            .map(|f| match &f.kind {
                FindingKind::Issue(i) => {
                    format!("{} {} {}", relative(root, &f.file), i.code, i.key)
                }
                _ => unreachable!(),
            })
            .collect();
        out.sort();
        out
    }

    /// A workspace with packages `@x/<name>` in `packages/<name>`, each
    /// with `src/index.ts`.
    fn workspace(packages: &[(&str, &str, &str)]) -> (TempDir, PathBuf) {
        let t = TempDir::new().unwrap();
        let root = t.path().canonicalize().unwrap();
        write(&root, "package.json", r#"{ "workspaces": ["packages/*"] }"#);
        for (name, pkg, tsconfig) in packages {
            let dir = format!("packages/{name}");
            let pkg = if pkg.is_empty() {
                format!(r#"{{ "name": "@x/{name}", "types": "./src/index.ts" }}"#)
            } else {
                pkg.to_string()
            };
            write(&root, &format!("{dir}/package.json"), &pkg);
            write(&root, &format!("{dir}/tsconfig.json"), tsconfig);
            write(&root, &format!("{dir}/src/index.ts"), "export {};");
        }
        (t, root)
    }

    const COMPOSITE: &str = r#"{ "compilerOptions": { "composite": true } }"#;

    #[test]
    fn paths_match_whole_directories() {
        let (_t, root) = workspace(&[("ui", "", COMPOSITE), ("ui-kit", "", COMPOSITE)]);
        write(&root, "packages/ui-kit/dist/index.d.ts", "");
        let paths = r#"{ "compilerOptions": { "paths": {
            "ui": ["./packages/ui-kit/src/index.ts"],
            "kit": ["./packages/ui-kit/src"],
            "types": ["./packages/ui-kit/dist"]
        } } }"#;
        write(&root, "tsconfig.json", paths);
        let project = Project::load(&root);
        let configs = TsConfigs::load(&project);
        let program = configs.get(&root.join("tsconfig.json")).unwrap();
        let (ui, kit) = (root.join("packages/ui"), root.join("packages/ui-kit"));
        assert!(!reaches_source(&project, program, "ui", &ui));
        assert!(reaches_source(&project, program, "ui", &kit));
        assert!(reaches_source(&project, program, "kit", &kit));
        assert!(!reaches_source(&project, program, "types", &kit));
    }

    #[test]
    fn reads_types_like_tsc() {
        let bun = r#"{ "name": "@x/core", "exports": { ".": {
            "types": "./dist/index.d.ts", "bun": "./src/index.ts", "default": "./dist/index.js"
        } } }"#;
        let module =
            r#"{ "name": "@x/util", "types": "./dist/index.d.ts", "module": "./src/index.ts" }"#;
        let (_t, root) = workspace(&[
            ("core", bun, COMPOSITE),
            ("util", module, COMPOSITE),
            ("bare", r#"{ "name": "@x/bare" }"#, COMPOSITE),
        ]);
        write(&root, "packages/core/dist/index.d.ts", "");
        write(&root, "packages/util/dist/index.d.ts", "");
        write(&root, "packages/bare/index.ts", "");
        let app = r#"{ "compilerOptions": { "moduleResolution": "bundler" }, "references": [] }"#;
        write(&root, "tsconfig.json", app);
        let project = Project::load(&root);
        let configs = TsConfigs::load(&project);
        let program = configs.get(&root.join("tsconfig.json")).unwrap();
        let reaches =
            |n: &str| reaches_source(&project, program, n, &root.join("packages").join(n));
        assert!(!reaches("core"));
        assert!(!reaches("util"));
        assert!(reaches("bare"));
    }

    #[test]
    fn follows_references_and_local_imports() {
        let (_t, root) = workspace(&[
            ("core", "", COMPOSITE),
            (
                "ui",
                "",
                r#"{ "compilerOptions": { "composite": true }, "references": [{ "path": "../core" }] }"#,
            ),
            (
                "app",
                "",
                r#"{ "files": ["src/index.ts"], "references": [{ "path": "../ui" }, { "path": "../lib" }] }"#,
            ),
            ("lib", "", COMPOSITE),
        ]);
        write(&root, "packages/ui/src/index.ts", "import '@x/core';");
        // app reaches core through ui's reference, and lib only through a
        // file `files` doesn't list.
        write(
            &root,
            "packages/app/src/index.ts",
            "import '@x/ui'; import '@x/core'; export * from './util.js';",
        );
        write(&root, "packages/app/src/util.ts", "import '@x/lib';");
        assert_eq!(codes(&root), Vec::<String>::new());
    }

    #[test]
    fn reports_files_less_targets_and_unloaded_configs() {
        let (_t, root) = workspace(&[
            ("core", "", r#"{ "files": [], "references": [{ "path": "./tsconfig.lib.json" }] }"#),
            (
                "app",
                "",
                r#"{ "references": [{ "path": "../core" }, { "path": "../core/build.json" }] }"#,
            ),
        ]);
        write(&root, "packages/core/tsconfig.lib.json", COMPOSITE);
        write(&root, "packages/core/build.json", COMPOSITE);
        write(&root, "packages/app/src/index.ts", "import '@x/core';");
        assert_eq!(codes(&root), ["packages/core/tsconfig.json not-composite composite"]);
    }
}
