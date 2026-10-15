//! `tsconfig`: compiler options that make every `tsc` run slower than it
//! needs to be. Reported, as `code`:
//!
//! - `skip-lib-check`: programs that don't set `skipLibCheck`, so `tsc`
//!   checks every `.d.ts` file in `node_modules` on each run. The finding,
//!   and the fix, go on the config the programs extend, so one setting
//!   covers them all.
//! - `incremental`: a config a package.json script runs with `tsc` (not
//!   `tsc -b`) without `incremental` or `composite`, so each run starts
//!   from nothing instead of reusing a `.tsbuildinfo`.
//! - `debug-option`: `diagnostics`, `extendedDiagnostics`,
//!   `generateTrace`, `listFiles`, `listEmittedFiles`, `explainFiles` or
//!   `traceResolution` left on, which make `tsc` collect and print far more
//!   than it checks.

use std::{collections::BTreeMap, path::Path};

use oxiclean_core::{CheckResult, Finding, FindingKind, Issue, Project, Related, Result, fix::Fix};
use serde_json::{Value, json};
use tracing::info;

use crate::{
    config::{Config, DEFAULT_THRESHOLD, TSCONFIG_RULE},
    scripts::{TscRun, tsc_runs},
    tsconfig::{TsConfigFile, TsConfigs},
};

/// Options that only help debug `tsc` and slow every run.
const DEBUG_OPTIONS: &[&str] = &[
    "diagnostics",
    "extendedDiagnostics",
    "generateTrace",
    "listFiles",
    "listEmittedFiles",
    "explainFiles",
    "traceResolution",
];

/// How deep an `extends` chain is followed.
const MAX_EXTENDS: usize = 32;

/// Runs the `tsconfig` check with `cfg`.
///
/// # Errors
/// Fails if the root can't be found or read, or the config file is invalid.
pub fn run_tsconfig_check(cfg: &Config) -> Result<CheckResult> {
    info!("Starting {TSCONFIG_RULE} check");
    let workspace = cfg.common.workspace()?;
    let policy = workspace.policy(TSCONFIG_RULE, DEFAULT_THRESHOLD)?;
    let configs = TsConfigs::load(&workspace.project);
    let findings = check(&workspace.project, &configs);
    info!("{TSCONFIG_RULE} check complete. Found {} findings", findings.len());
    Ok(workspace.finish(&policy, findings, configs.configs.len()))
}

/// The findings for `configs` in `project`.
pub(crate) fn check(project: &Project, configs: &TsConfigs) -> Vec<Finding> {
    let mut findings = skip_lib_check(configs);
    findings.extend(incremental(project, configs));
    findings.extend(debug_options(configs));
    findings
}

/// The config at the far end of `config`'s `extends` chain in this repo,
/// following the last config each one extends, as it wins.
fn base<'a>(configs: &'a TsConfigs, config: &'a TsConfigFile) -> &'a TsConfigFile {
    let mut current = config;
    for _ in 0..MAX_EXTENDS {
        match current.extends.last().and_then(|p| configs.get(p)) {
            Some(next) => current = next,
            None => break,
        }
    }
    current
}

/// Programs with files that leave `skipLibCheck` unset, by the config
/// they extend.
fn skip_lib_check(configs: &TsConfigs) -> Vec<Finding> {
    let mut by_base: BTreeMap<&Path, (&TsConfigFile, Vec<&TsConfigFile>)> = BTreeMap::new();
    for program in configs.programs() {
        // A base that isn't installed may set it.
        if program.root_files.is_empty()
            || program.partial
            || program.flag("skipLibCheck").is_some()
        {
            continue;
        }
        let b = base(configs, program);
        by_base.entry(b.path.as_path()).or_insert_with(|| (b, Vec::new())).1.push(program);
    }
    by_base
        .into_values()
        .map(|(b, programs)| {
            let message = if programs.len() == 1 && programs[0].path == b.path {
                "doesn't set skipLibCheck, so tsc type-checks every .d.ts in node_modules on each run".to_string()
            } else {
                format!(
                    "doesn't set skipLibCheck, so tsc type-checks every .d.ts in node_modules on each run of the {} programs that extend it",
                    programs.len()
                )
            };
            let related = programs
                .iter()
                .filter(|p| p.path != b.path)
                .map(|p| Related::new(p.path.clone(), p.line_of(&["extends"]), "inherits it"))
                .collect();
            let issue = Issue::new("skip-lib-check", "skipLibCheck", message, b.line_of(&["compilerOptions"]))
                .with_related(related)
                .with_fix(Fix::json_set(b.path.clone(), &["compilerOptions", "skipLibCheck"], json!(true)));
            Finding::new(b.path.clone(), FindingKind::Issue(issue), programs.len())
        })
        .collect()
}

/// Configs scripts run with plain `tsc` that don't keep a `.tsbuildinfo`.
fn incremental(project: &Project, configs: &TsConfigs) -> Vec<Finding> {
    tsc_runs(project)
        .into_iter()
        .filter(|(_, run)| *run == TscRun::Program)
        .filter_map(|(path, _)| configs.get(&path))
        .filter(|c| {
            !c.root_files.is_empty()
                && !c.partial
                && c.flag("incremental") != Some(true)
                && c.flag("composite") != Some(true)
        })
        .map(|c| {
            let message = "is run by a package.json script with tsc but isn't incremental, so each run checks everything again instead of reusing a .tsbuildinfo";
            let issue = Issue::new("incremental", "incremental", message, c.line_of(&["compilerOptions"]))
                .with_fix(Fix::json_set(c.path.clone(), &["compilerOptions", "incremental"], json!(true)));
            Finding::new(c.path.clone(), FindingKind::Issue(issue), 1)
        })
        .collect()
}

/// Debug options configs turn on themselves.
fn debug_options(configs: &TsConfigs) -> Vec<Finding> {
    let mut findings = Vec::new();
    for c in &configs.configs {
        for &option in DEBUG_OPTIONS {
            let on = match c.options.get(option) {
                Some(Value::Bool(b)) => *b,
                Some(Value::String(s)) => !s.is_empty(),
                _ => false,
            };
            if !on || !c.sets_option(option) {
                continue;
            }
            let message = format!(
                "turns on {option}, which makes every tsc run slower; pass it on the command line when debugging instead"
            );
            let line = c.line_of(&["compilerOptions", option]);
            let mut issue = Issue::new("debug-option", option, message, line);
            // `generateTrace` takes a directory; the others turn off.
            if option != "generateTrace" {
                issue = issue.with_fix(Fix::json_set(
                    c.path.clone(),
                    &["compilerOptions", option],
                    json!(false),
                ));
            }
            findings.push(Finding::new(c.path.clone(), FindingKind::Issue(issue), 1));
        }
    }
    findings
}
