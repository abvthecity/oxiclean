//! `ambient-types`: programs that load every `@types` package they can
//! see.
//!
//! Before TypeScript 6, a program without `compilerOptions.types` loads
//! every package in each `node_modules/@types` from its directory up,
//! whether or not anything uses it; from 6 on, only with `"types": ["*"]`.
//! In a monorepo that is every `@types` package any workspace package
//! installs and the package manager hoists. A program is reported when that
//! includes packages that no package.json or deno.json from its directory
//! up declares, nor the `@types` packages those depend on.
//!
//! There is no fix: a program may use the globals of a package only a
//! sibling declares, and listing `types` would break it.

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use oxiclean_core::{
    CheckResult, Finding, FindingKind, Issue, Project, Related, Result, parse_jsonc,
};
use serde_json::Value;
use tracing::info;

use crate::{
    config::{AMBIENT_TYPES_RULE, Config, DEFAULT_THRESHOLD},
    tsconfig::{TsConfigFile, TsConfigs},
    typescript::{DEPENDENCY_FIELDS, typescript_version},
};

/// How many undeclared packages a finding names.
const SAMPLES: usize = 3;

/// The first TypeScript major version whose `types` defaults to `[]`.
const TYPES_DEFAULT_EMPTY: u64 = 6;

/// Runs the `ambient-types` check with `cfg`.
///
/// # Errors
/// Fails if the root can't be found or read, or the config file is invalid.
pub fn run_ambient_types_check(cfg: &Config) -> Result<CheckResult> {
    info!("Starting {AMBIENT_TYPES_RULE} check");
    let workspace = cfg.common.workspace()?;
    let policy = workspace.policy(AMBIENT_TYPES_RULE, DEFAULT_THRESHOLD)?;
    let configs = TsConfigs::load(&workspace.project);
    let findings = check(&workspace.project, &configs);
    info!("{AMBIENT_TYPES_RULE} check complete. Found {} findings", findings.len());
    Ok(workspace.finish(&policy, findings, configs.programs().count()))
}

/// The findings for the programs in `configs`.
pub(crate) fn check(project: &Project, configs: &TsConfigs) -> Vec<Finding> {
    let mut findings = Vec::new();
    for program in configs.programs() {
        if program.root_files.is_empty()
            || program.partial
            || program.options.contains_key("typeRoots")
            || !loads_all_types(project, program)
        {
            continue;
        }
        let dirs: Vec<&Path> =
            program.dir.ancestors().take_while(|a| a.starts_with(&project.root)).collect();
        let visible = visible_types(&dirs, &project.root);
        let declared = declared_types(project, &dirs, &visible);
        let (used, extra): (Vec<&String>, Vec<&String>) =
            visible.iter().map(|(name, _)| name).partition(|t| declared.contains(*t));
        if extra.is_empty() {
            continue;
        }
        let names: Vec<String> =
            extra.iter().take(SAMPLES).map(|t| format!("@types/{t}")).collect();
        let more = extra.len().saturating_sub(SAMPLES);
        let listed: Vec<String> = used.iter().map(|t| format!("\"{t}\"")).collect();
        let message = format!(
            "loads every @types package it can see, including {}{} that no package.json here declares; list the ones it uses in compilerOptions.types, such as [{}]",
            names.join(", "),
            if more > 0 { format!(" and {more} more") } else { String::new() },
            listed.join(", ")
        );
        // SARIF locations must be files, so each package by its package.json
        // or entry.
        let related = extra
            .iter()
            .take(SAMPLES)
            .filter_map(|t| {
                let dir = &visible.iter().find(|(n, _)| n == *t)?.1;
                let file = ["package.json", "index.d.ts"]
                    .iter()
                    .map(|f| dir.join(f))
                    .find(|f| f.is_file())?;
                Some(Related::new(file, 0, "not declared"))
            })
            .collect();
        let line = program.line_of(&["compilerOptions"]);
        let issue = Issue::new("undeclared-types", "types", message, line).with_related(related);
        findings.push(Finding::new(program.path.clone(), FindingKind::Issue(issue), extra.len()));
    }
    findings
}

/// Whether `program` loads every visible `@types` package: it sets no
/// `types` and runs on TypeScript before 6, or its `types` has `"*"`.
pub(crate) fn loads_all_types(project: &Project, program: &TsConfigFile) -> bool {
    match program.options.get("types") {
        Some(Value::Array(types)) => types.iter().any(|t| t == "*"),
        Some(_) => false,
        None => typescript_version(project, &program.dir)
            .is_none_or(|(major, _)| major < TYPES_DEFAULT_EMPTY),
    }
}

/// The `@types` packages, without the scope, in each `node_modules/@types`
/// of `dirs`, nearest first, with where each is.
pub(crate) fn visible_types(dirs: &[&Path], root: &Path) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    for d in dirs {
        let Ok(entries) = fs::read_dir(d.join("node_modules/@types")) else { continue };
        let mut found: Vec<(String, PathBuf)> = entries
            .flatten()
            .filter(|e| e.path().is_dir())
            .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
            // `tsc` skips dot-folders.
            .filter(|(n, _)| !n.starts_with('.') && !out.iter().any(|(o, _)| o == n))
            .collect();
        found.sort();
        out.extend(found);
        if *d == root {
            break;
        }
    }
    out
}

/// The `@types` packages, without the scope, that `dirs` declare: in a
/// package.json, as `npm:@types/` imports in a deno.json, or installed in a
/// package's own `node_modules` rather than hoisted to the root; and the
/// `@types` packages those depend on, from `visible`.
fn declared_types(
    project: &Project,
    dirs: &[&Path],
    visible: &[(String, PathBuf)],
) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for d in dirs {
        if let Some(pkg) = project.package_json(d) {
            out.extend(type_dependencies(&pkg));
        }
        for name in ["deno.json", "deno.jsonc"] {
            let Some(deno) = fs::read_to_string(d.join(name)).ok().and_then(|t| parse_jsonc(&t))
            else {
                continue;
            };
            let imports = deno.get("imports").and_then(Value::as_object).into_iter().flatten();
            out.extend(imports.filter_map(|(_, v)| {
                let spec = v.as_str()?.strip_prefix("npm:@types/")?;
                // `npm:@types/node@^22` names `node`.
                Some(spec.split('@').next().unwrap_or(spec).to_string())
            }));
        }
    }
    let root = dirs.last().copied();
    out.extend(
        visible
            .iter()
            .filter(|(_, dir)| !root.is_some_and(|r| dir.starts_with(r.join("node_modules"))))
            .map(|(n, _)| n.clone()),
    );
    // What declared `@types` packages depend on is loaded either way.
    let mut queue: Vec<String> = out.iter().cloned().collect();
    while let Some(name) = queue.pop() {
        let Some((_, dir)) = visible.iter().find(|(n, _)| *n == name) else { continue };
        let Some(pkg) = fs::read_to_string(dir.join("package.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        else {
            continue;
        };
        for dep in type_dependencies(&pkg) {
            if out.insert(dep.clone()) {
                queue.push(dep);
            }
        }
    }
    out
}

/// The `@types` packages, without the scope, a package.json depends on.
fn type_dependencies(pkg: &Value) -> Vec<String> {
    DEPENDENCY_FIELDS
        .iter()
        .filter_map(|f| pkg.get(f).and_then(Value::as_object))
        .flat_map(|deps| deps.keys())
        .filter_map(|k| k.strip_prefix("@types/"))
        .map(str::to_string)
        .collect()
}
