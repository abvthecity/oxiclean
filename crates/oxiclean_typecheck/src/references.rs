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
    collections::{BTreeMap, BTreeSet, HashMap},
    path::{Path, PathBuf},
};

use oxiclean_core::{
    CheckResult, Finding, FindingKind, Issue, Project, Related, Result,
    fix::{Fix, FixOp},
    jsonc_tree::Segment,
    parse_imports, sequence, split_package_request,
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
    let uses_references = configs
        .configs
        .iter()
        .any(|c| !c.references.is_empty() || c.flag("composite") == Some(true));
    let mut findings = Vec::new();
    let mut missing = BTreeSet::new();
    if uses_references {
        let (mismatches, reported) = reference_mismatches(project, configs, &imports);
        findings.extend(mismatches);
        missing = reported;
        findings.extend(reference_targets(configs));
        findings.extend(reference_cycles(configs));
    }
    findings.extend(source_imports(project, configs, &imports, &missing));
    findings
}

/// Whether `program` type-checks the source of the workspace package
/// `name` in `dir` when it imports it, rather than built `.d.ts` files.
fn reaches_source(project: &Project, program: &TsConfigFile, name: &str, dir: &Path) -> bool {
    exposes_source(project, dir) || paths_into_source(program, name, dir)
}

/// For each program, the workspace packages its files import, other than
/// its own, with where each is first imported.
fn package_imports(
    project: &Project,
    configs: &TsConfigs,
) -> HashMap<PathBuf, BTreeMap<String, ImportSite>> {
    let files: BTreeSet<&Path> =
        configs.programs().flat_map(|p| p.root_files.iter().map(PathBuf::as_path)).collect();
    let by_file: HashMap<&Path, Vec<(String, u32)>> = files
        .into_par_iter()
        .map(|f| {
            let specs = match parse_imports(f) {
                Ok(parsed) => parsed.specs,
                Err(e) => {
                    warn!("Skipping {}: {e}", f.display());
                    Vec::new()
                }
            };
            let packages = specs
                .into_iter()
                .filter(|s| !s.request.starts_with('.') && !s.request.starts_with('/'))
                .filter_map(|s| {
                    let (name, _) = split_package_request(&s.request);
                    project.workspace_package(name).map(|_| (name.to_string(), s.line))
                })
                .collect();
            (f, packages)
        })
        .collect();
    configs
        .programs()
        .map(|p| {
            let own = project.package_for(&p.path);
            let mut map: BTreeMap<String, ImportSite> = BTreeMap::new();
            for f in &p.root_files {
                for (name, line) in by_file.get(f.as_path()).into_iter().flatten() {
                    if Some(name.as_str()) != own && !map.contains_key(name) {
                        let source = project
                            .workspace_package(name)
                            .is_some_and(|dir| reaches_source(project, p, name, dir));
                        map.insert(
                            name.clone(),
                            ImportSite { file: f.clone(), line: *line, source },
                        );
                    }
                }
            }
            (p.path.clone(), map)
        })
        .collect()
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
        let own_dir = project.package_for(&program.path).and_then(|n| project.workspace_package(n));
        for (name, site) in imported {
            let Some(dir) = project.workspace_package(name) else { continue };
            // An import that resolves to built `.d.ts` files costs nothing
            // to check, whatever builds them; one that reaches source needs
            // a reference to read the `.d.ts` instead.
            let Some(target) = reference_target(configs, dir) else { continue };
            if program.references.iter().any(|r| r.target.starts_with(dir)) || !site.source {
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
        for r in &program.references {
            let Some(target_dir) = r.target.parent() else { continue };
            // Only references to other workspace packages: a solution
            // config's references to its own variants are structure.
            let Some(name) = project.package_for(&r.target) else { continue };
            if own_dir.is_some_and(|d| target_dir.starts_with(d)) || imported.contains_key(name) {
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
                None => {
                    let message = format!("references {}, which doesn't exist", r.raw);
                    let issue = Issue::new("missing-target", r.raw.clone(), message, r.line);
                    findings.push(Finding::new(c.path.clone(), FindingKind::Issue(issue), 1));
                }
                // A solution config with no files of its own, such as Vite's
                // `tsconfig.json`, may reference configs that aren't
                // composite.
                Some(t)
                    if t.flag("composite") != Some(true)
                        && !t.root_files.is_empty()
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
    missing: &BTreeSet<(&Path, &str)>,
) -> Vec<Finding> {
    // Package name -> its directory and the consumer programs that reach
    // its source, other than those that reference it or already have a
    // missing reference reported. The value counts these configs.
    let mut consumers: BTreeMap<&str, (&Path, BTreeMap<&str, &TsConfigFile>)> = BTreeMap::new();
    for program in configs.programs() {
        let Some(imported) = imports.get(&program.path) else { continue };
        let consumer = project.package_for(&program.path).unwrap_or(".");
        for (name, site) in imported {
            let Some(dir) = project.workspace_package(name) else { continue };
            let referenced = program.references.iter().any(|r| r.target.starts_with(dir));
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

/// Whether the package in `dir` points its types at TypeScript source:
/// `types`, `typings`, `main`, `module` or an `exports` target ending in
/// `.ts` but not `.d.ts`.
fn exposes_source(project: &Project, dir: &Path) -> bool {
    let Some(pkg) = project.package_json(dir) else { return false };
    let mut targets: Vec<&str> = ["types", "typings", "main", "module"]
        .iter()
        .filter_map(|k| pkg.get(k)?.as_str())
        .collect();
    if let Some(exports) = pkg.get("exports") {
        strings(exports, &mut targets);
    }
    targets.iter().any(|t| is_source(t))
}

/// Every string in `v`.
fn strings<'a>(v: &'a Value, out: &mut Vec<&'a str>) {
    match v {
        Value::String(s) => out.push(s),
        Value::Array(items) => items.iter().for_each(|i| strings(i, out)),
        Value::Object(map) => map.values().for_each(|i| strings(i, out)),
        _ => {}
    }
}

/// Whether `program`'s `paths` map `name` into TypeScript source in `dir`:
/// an alias for the package, its subpaths, or a wildcard matching it.
fn paths_into_source(program: &TsConfigFile, name: &str, dir: &Path) -> bool {
    let Some(Value::Object(paths)) = program.options.get("paths") else { return false };
    let dir = slashes(dir);
    paths.iter().any(|(key, targets)| {
        let star = match key.split_once('*') {
            None => (key == name).then_some(""),
            Some((prefix, suffix)) => name
                .strip_prefix(prefix)
                .and_then(|rest| rest.strip_suffix(suffix))
                .or_else(|| key.strip_prefix(name).filter(|r| r.starts_with('/')).map(|_| "")),
        };
        let Some(star) = star else { return false };
        targets.as_array().into_iter().flatten().filter_map(Value::as_str).any(|t| {
            let t = t.replacen('*', star, 1);
            // A file in `dir`: TypeScript source, or a path with no
            // extension that resolves to one.
            Path::new(&t).strip_prefix(&dir).is_ok_and(|rest| {
                let name = rest.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
                !is_declaration(&name) && (is_source(&name) || !name.contains('.'))
            })
        })
    })
}

/// Whether `target` names a TypeScript source file rather than a
/// declaration file.
fn is_source(target: &str) -> bool {
    let t = target.trim_end_matches('*');
    [".ts", ".tsx", ".mts", ".cts"].iter().any(|e| t.ends_with(e)) && !is_declaration(t)
}

/// Whether `target` names a declaration file: `.d.ts`, `.d.mts`,
/// `.d.cts`, or an arbitrary-extension `.d.<ext>.ts`.
fn is_declaration(target: &str) -> bool {
    let name = target.rsplit('/').next().unwrap_or(target);
    name.contains(".d.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn paths_match_whole_directories() {
        let t = TempDir::new().unwrap();
        let root = t.path().canonicalize().unwrap();
        let paths = r#"{ "compilerOptions": { "paths": {
            "ui": ["./packages/ui-kit/src/index.ts"],
            "kit": ["./packages/ui-kit/src"],
            "types": ["./packages/ui-kit/dist/index.d.ts"]
        } } }"#;
        fs::write(root.join("tsconfig.json"), paths).unwrap();
        let configs = TsConfigs::load(&Project::load(&root));
        let program = configs.programs().next().unwrap();
        let (ui, kit) = (root.join("packages/ui"), root.join("packages/ui-kit"));
        assert!(!paths_into_source(program, "ui", &ui));
        assert!(paths_into_source(program, "ui", &kit));
        assert!(paths_into_source(program, "kit", &kit));
        assert!(!paths_into_source(program, "types", &kit));
    }
}
