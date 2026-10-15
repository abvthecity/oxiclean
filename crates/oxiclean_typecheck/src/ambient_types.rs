//! `ambient-types`: programs that load every `@types` package they can
//! see.
//!
//! Without `compilerOptions.types`, `tsc` loads every package in each
//! `node_modules/@types` from the program's directory up, whether or not
//! anything uses it. In a monorepo that is every `@types` package any
//! workspace package installs and the package manager hoists. A program is
//! reported when that includes packages neither its own package.json nor
//! the root one declares; the fix sets `types` to the ones they do
//! declare, which imports of other packages' types don't need.

use std::{collections::BTreeSet, fs, path::Path};

use oxiclean_core::{CheckResult, Finding, FindingKind, Issue, Project, Related, Result, fix::Fix};
use serde_json::{Value, json};
use tracing::info;

use crate::{
    config::{AMBIENT_TYPES_RULE, Config, DEFAULT_THRESHOLD},
    tsconfig::TsConfigs,
};

/// How many undeclared packages a message names.
const SAMPLES: usize = 3;

/// The package.json fields that declare dependencies.
const DEPENDENCY_FIELDS: &[&str] =
    &["dependencies", "devDependencies", "peerDependencies", "optionalDependencies"];

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
    let root_declared = declared(project, &project.root);
    let mut findings = Vec::new();
    for program in configs.programs() {
        if program.root_files.is_empty()
            || program.partial
            || program.options.contains_key("types")
            || program.options.contains_key("typeRoots")
        {
            continue;
        }
        let visible = visible(&project.root, &program.dir);
        let mut declared = root_declared.clone();
        if let Some(dir) =
            project.package_for(&program.path).and_then(|n| project.workspace_package(n))
        {
            declared.extend(self::declared(project, dir));
        }
        let extra: Vec<&String> = visible.iter().filter(|t| !declared.contains(*t)).collect();
        if extra.is_empty() {
            continue;
        }
        let keep: Vec<&String> = visible.iter().filter(|t| declared.contains(*t)).collect();
        let names: Vec<String> =
            extra.iter().take(SAMPLES).map(|t| format!("@types/{t}")).collect();
        let more = extra.len().saturating_sub(SAMPLES);
        let message = format!(
            "sets no compilerOptions.types, so tsc loads every @types package it can see, including {}{} that no package.json here declares",
            names.join(", "),
            if more > 0 { format!(" and {more} more") } else { String::new() }
        );
        let related = extra
            .iter()
            .take(SAMPLES)
            .filter_map(|t| {
                let dir = program
                    .dir
                    .ancestors()
                    .take_while(|a| a.starts_with(&project.root))
                    .map(|a| a.join("node_modules/@types").join(t))
                    .find(|d| d.is_dir())?;
                // Locations name files: its package.json, else its entry.
                let file = ["package.json", "index.d.ts"]
                    .iter()
                    .map(|f| dir.join(f))
                    .find(|f| f.is_file())?;
                Some(Related::new(file, 0, "not declared"))
            })
            .collect();
        let line = program.line_of(&["compilerOptions"]);
        let issue =
            Issue::new("undeclared-types", "types", message, line).with_related(related).with_fix(
                Fix::json_set(program.path.clone(), &["compilerOptions", "types"], json!(keep)),
            );
        findings.push(Finding::new(program.path.clone(), FindingKind::Issue(issue), extra.len()));
    }
    findings
}

/// The `@types` packages, without the scope, in each `node_modules/@types`
/// from `dir` up to `root`, as `tsc` finds them.
fn visible(root: &Path, dir: &Path) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for a in dir.ancestors().take_while(|a| a.starts_with(root)) {
        let Ok(entries) = fs::read_dir(a.join("node_modules/@types")) else { continue };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            // `tsc` skips dot-folders, and a package needs a directory.
            if !name.starts_with('.') && e.path().is_dir() {
                out.insert(name);
            }
        }
    }
    out
}

/// The `@types` packages, without the scope, the package.json in `dir`
/// declares.
fn declared(project: &Project, dir: &Path) -> BTreeSet<String> {
    let Some(pkg) = project.package_json(dir) else { return BTreeSet::new() };
    DEPENDENCY_FIELDS
        .iter()
        .filter_map(|f| pkg.get(f).and_then(Value::as_object))
        .flat_map(|deps| deps.keys())
        .filter_map(|k| k.strip_prefix("@types/"))
        .map(str::to_string)
        .collect()
}
