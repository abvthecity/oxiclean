//! `tsconfig`: compiler options that make every `tsc` run slower than it
//! needs to be. Reported, as `code`:
//!
//! - `skip-lib-check`: programs that don't set `skipLibCheck`, so `tsc`
//!   checks every `.d.ts` file in `node_modules` on each run. The finding,
//!   and the fix, go on the config the programs extend, so one setting
//!   covers them all. Programs made mostly of `.d.ts` files exist to check
//!   them and aren't reported.
//! - `incremental`: a config a package.json script only checks with
//!   `tsc` (`noEmit`, and not `tsc -b` or `--incremental`) that doesn't set
//!   `incremental` or `composite`, so each run starts from nothing instead
//!   of reusing a `.tsbuildinfo`. Configs that emit aren't reported: a
//!   `.tsbuildinfo` outside the output directory makes
//!   `rm -rf dist && tsc` emit nothing.
//! - `debug-option`: `diagnostics`, `extendedDiagnostics`,
//!   `generateTrace`, `listFiles`, `listEmittedFiles`, `explainFiles` or
//!   `traceResolution` left on, which make `tsc` collect and print far more
//!   than it checks.
//!
//! Configs whose `extends` chain can't be read are skipped, as what they
//! inherit may set these options.

use std::{collections::BTreeMap, path::Path};

use oxiclean_core::{
    CheckResult, Finding, FindingKind, Issue, Project, Related, Result, Severity, Workspace,
    fix::{Fix, FixOp},
    jsonc_tree::Segment,
};
use serde_json::{Value, json};
use tracing::info;

use crate::{
    config::{Config, DEFAULT_THRESHOLD, TSCONFIG_RULE},
    scripts::{TscRun, tsc_runs},
    tsconfig::{MAX_EXTENDS_DEPTH, TsConfigFile, TsConfigs},
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

/// Words in the names of configs kept for debugging `tsc`, such as
/// `tsconfig.explain.json`, whose debug options are their point.
const DEBUG_CONFIG_WORDS: &[&str] = &["debug", "diag", "explain", "trace"];

/// Runs the `tsconfig` check with `cfg`.
///
/// # Errors
/// Fails if the root can't be found or read, or the config file is invalid.
pub fn run_tsconfig_check(cfg: &Config) -> Result<CheckResult> {
    let workspace = cfg.common.workspace()?;
    run_on(&workspace, &TsConfigs::load(&workspace.project))
}

/// Runs the check on `workspace`, whose tsconfig files are `configs`.
pub(crate) fn run_on(workspace: &Workspace, configs: &TsConfigs) -> Result<CheckResult> {
    info!("Starting {TSCONFIG_RULE} check");
    let policy = workspace.policy(TSCONFIG_RULE, DEFAULT_THRESHOLD, Severity::Error)?;
    let findings = check(&workspace.project, configs);
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
    for _ in 0..MAX_EXTENDS_DEPTH {
        match current.extends.last().and_then(|p| configs.get(p)) {
            Some(next) => current = next,
            None => break,
        }
    }
    current
}

/// Whether most of `program`'s files are declaration files, which it is
/// there to check: `skipLibCheck` would skip them too.
fn checks_declarations(program: &TsConfigFile) -> bool {
    let declarations = program
        .root_files
        .iter()
        .filter(|f| f.file_name().is_some_and(|n| n.to_string_lossy().contains(".d.")))
        .count();
    declarations * 2 > program.root_files.len()
}

/// Programs with files that leave `skipLibCheck` unset, by the config
/// they extend.
fn skip_lib_check(configs: &TsConfigs) -> Vec<Finding> {
    let mut by_base: BTreeMap<&Path, (&TsConfigFile, Vec<&TsConfigFile>)> = BTreeMap::new();
    for program in configs.programs() {
        if program.root_files.is_empty()
            || program.partial
            || program.flag("skipLibCheck").is_some()
            || checks_declarations(program)
        {
            continue;
        }
        let b = base(configs, program);
        by_base.entry(b.path.as_path()).or_insert_with(|| (b, Vec::new())).1.push(program);
    }
    by_base
        .into_values()
        .map(|(b, programs)| {
            let others: Vec<&&TsConfigFile> = programs.iter().filter(|p| p.path != b.path).collect();
            let whose = match others.len() {
                0 => String::new(),
                1 => " of the program that extends it".to_string(),
                n => format!(" of the {n} programs that extend it"),
            };
            let message = format!(
                "doesn't set skipLibCheck, so tsc type-checks every .d.ts in node_modules on each run{whose}; with it, tsc skips your own .d.ts files too"
            );
            let related = others
                .iter()
                .map(|p| Related::new(p.path.clone(), p.line_of(&["extends"]), "inherits it"))
                .collect();
            let line = b.line_of(&["compilerOptions"]);
            let fix = Fix::json_set(b.path.clone(), &["compilerOptions", "skipLibCheck"], json!(true));
            let issue = Issue::new("skip-lib-check", "skipLibCheck", message, line)
                .with_related(related)
                .with_fix(fix);
            Finding::new(b.path.clone(), FindingKind::Issue(issue), programs.len())
        })
        .collect()
}

/// `noEmit` configs scripts run with plain `tsc` that don't keep a
/// `.tsbuildinfo`. The fix puts it in `node_modules/.tmp`, as Vite's
/// templates do, so it stays out of git.
fn incremental(project: &Project, configs: &TsConfigs) -> Vec<Finding> {
    tsc_runs(project)
        .into_iter()
        .filter_map(|(path, run)| Some((configs.get(&path)?, run)))
        .filter(|(c, run)| {
            let checks_only = *run == TscRun::Check
                || (*run == TscRun::Program && c.flag("noEmit") == Some(true));
            checks_only
                && !c.root_files.is_empty()
                && !c.partial
                && c.flag("incremental").is_none()
                && c.flag("composite").is_none()
        })
        .map(|(c, _)| {
            let message = "is run by a package.json script with tsc but isn't incremental, so each run checks everything again instead of reusing a .tsbuildinfo";
            let set = |option: &str, value: Value| FixOp::JsonSet {
                path: vec![Segment::from("compilerOptions"), Segment::from(option)],
                value,
            };
            let mut ops = vec![set("incremental", json!(true))];
            let mut description = "set \"compilerOptions.incremental\": true".to_string();
            if !c.options.contains_key("tsBuildInfoFile") {
                let stem = c.name().trim_end_matches(".json").to_string();
                let file = format!("./node_modules/.tmp/{stem}.tsbuildinfo");
                description.push_str(&format!(" and \"tsBuildInfoFile\": \"{file}\""));
                ops.push(set("tsBuildInfoFile", json!(file)));
            }
            let issue = Issue::new("incremental", "incremental", message, c.line_of(&["compilerOptions"]))
                .with_fix(Fix::new(c.path.clone(), description, ops));
            Finding::new(c.path.clone(), FindingKind::Issue(issue), 1)
        })
        .collect()
}

/// Debug options configs turn on themselves.
fn debug_options(configs: &TsConfigs) -> Vec<Finding> {
    let mut findings = Vec::new();
    for c in &configs.configs {
        let name = c.name().to_lowercase();
        if DEBUG_CONFIG_WORDS.iter().any(|w| name.contains(w)) {
            continue;
        }
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
            // `generateTrace` takes a directory and has no off value.
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
