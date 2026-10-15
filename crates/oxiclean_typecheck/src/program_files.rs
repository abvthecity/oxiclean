//! `program-files`: files a tsconfig program type-checks that nobody needs
//! checked.
//!
//! - Build output: files in a tsconfig's `outDir` or `declarationDir`. A
//!   program only excludes its own `outDir`, and only when it sets no
//!   `exclude`, so another config's output (`dist` from
//!   `tsconfig.build.json`, say) is checked again as if it were source.
//! - Minified or very large JavaScript pulled in by `allowJs`, such as
//!   vendored bundles.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use oxiclean_core::{
    CheckResult, Finding, FindingKind, Issue, Related, Result,
    fix::{Fix, FixOp},
    jsonc_tree::Segment,
};
use serde_json::Value;
use tracing::info;

use crate::{
    config::{Config, DEFAULT_THRESHOLD, PROGRAM_FILES_RULE},
    tsconfig::{TsConfigFile, TsConfigs, relative},
};

/// JavaScript files at least this big are reported when `allowJs` pulls
/// them in.
const LARGE_JS_BYTES: u64 = 1024 * 1024;

/// How many of the files a finding is about are listed with it.
const SAMPLES: usize = 3;

/// Runs the `program-files` check with `cfg`.
///
/// # Errors
/// Fails if the root can't be found or read, or the config file is invalid.
pub fn run_program_files_check(cfg: &Config) -> Result<CheckResult> {
    info!("Starting {PROGRAM_FILES_RULE} check");
    let workspace = cfg.common.workspace()?;
    let policy = workspace.policy(PROGRAM_FILES_RULE, DEFAULT_THRESHOLD)?;
    let configs = TsConfigs::load(&workspace.project);
    let findings = check(&configs);
    let programs = configs.programs().count();
    info!("{PROGRAM_FILES_RULE} check complete. Found {} findings", findings.len());
    Ok(workspace.finish(&policy, findings, programs))
}

/// The findings for every program in `configs`.
#[must_use]
pub fn check(configs: &TsConfigs) -> Vec<Finding> {
    // Every output directory, with the config that emits to it.
    let mut outputs: BTreeMap<PathBuf, &TsConfigFile> = BTreeMap::new();
    for c in &configs.configs {
        for d in c.output_dirs() {
            outputs.entry(d).or_insert(c);
        }
    }
    let mut findings = Vec::new();
    for program in configs.programs() {
        let mut by_dir: BTreeMap<&Path, Vec<&PathBuf>> = BTreeMap::new();
        for f in &program.root_files {
            // The deepest output directory holding the file.
            if let Some((dir, _)) = outputs.iter().rev().find(|(d, _)| f.starts_with(d)) {
                by_dir.entry(dir.as_path()).or_default().push(f);
            }
        }
        for (dir, files) in by_dir {
            findings.push(build_output(program, dir, outputs[dir], &files));
        }
        if let Some(f) = large_js(program) {
            findings.push(f);
        }
    }
    findings
}

/// A finding for `files` of `program` that are in `dir`, where `emitter`
/// writes its output.
fn build_output(
    program: &TsConfigFile,
    dir: &Path,
    emitter: &TsConfigFile,
    files: &[&PathBuf],
) -> Finding {
    let rel = relative(&program.dir, dir);
    let whose = if emitter.path == program.path {
        "its own output directory".to_string()
    } else {
        format!("the output directory of {}", relative(&program.dir, &emitter.path))
    };
    let message = format!("type-checks the build output in {rel}, {whose}, as if it were source");
    let option = if emitter.path_option("outDir").as_deref() == Some(dir) {
        "outDir"
    } else {
        "declarationDir"
    };
    let mut related = vec![Related::new(
        emitter.path.clone(),
        emitter.line_of(&["compilerOptions", option]),
        format!("emits to {}", relative(&emitter.dir, dir)),
    )];
    related
        .extend(files.iter().take(SAMPLES).map(|f| Related::new((*f).clone(), 0, "build output")));
    let issue = Issue::new("build-output", rel.clone(), message, include_line(program))
        .with_related(related)
        .with_fix(exclude_fix(program, &[rel]));
    Finding::new(program.path.clone(), FindingKind::Issue(issue), files.len())
}

/// A finding for the minified or very large JavaScript files `program`
/// checks with `allowJs`, if any.
fn large_js(program: &TsConfigFile) -> Option<Finding> {
    if !program.flag("allowJs").unwrap_or(false) {
        return None;
    }
    let js = |f: &Path| f.extension().is_some_and(|e| e == "js" || e == "mjs" || e == "cjs");
    let minified = |f: &Path| f.file_name().is_some_and(|n| n.to_string_lossy().contains(".min."));
    let files: Vec<&PathBuf> = program
        .root_files
        .iter()
        .filter(|f| js(f))
        .filter(|f| minified(f) || fs::metadata(f).is_ok_and(|m| m.len() >= LARGE_JS_BYTES))
        .collect();
    if files.is_empty() {
        return None;
    }
    let mut patterns: Vec<String> = Vec::new();
    for f in &files {
        let p = if minified(f) {
            format!("**/*.min.{}", f.extension().unwrap_or_default().to_string_lossy())
        } else {
            relative(&program.dir, f)
        };
        if !patterns.contains(&p) {
            patterns.push(p);
        }
    }
    let message = "type-checks minified or very large JavaScript that allowJs pulls in".to_string();
    let related = files
        .iter()
        .take(SAMPLES)
        .map(|f| Related::new((*f).clone(), 0, "large JavaScript"))
        .collect();
    let issue = Issue::new("large-js", "allowJs", message, include_line(program))
        .with_related(related)
        .with_fix(exclude_fix(program, &patterns));
    Some(Finding::new(program.path.clone(), FindingKind::Issue(issue), files.len()))
}

/// The line a finding about what `program` includes points at.
fn include_line(program: &TsConfigFile) -> u32 {
    ["include", "files", "exclude", "extends"]
        .iter()
        .map(|k| program.line_of(&[k]))
        .find(|l| *l > 0)
        .unwrap_or(0)
}

/// A fix adding `patterns` to `program`'s `exclude`. When the file sets no
/// `exclude` of its own, it gets one that keeps what it excluded before:
/// the inherited list, or else `tsc`'s default of its own `outDir`.
pub(crate) fn exclude_fix(program: &TsConfigFile, patterns: &[String]) -> Fix {
    let file = program.path.clone();
    let description = format!("add {} to \"exclude\"", quoted(patterns));
    let key = vec![Segment::from("exclude")];
    if program.sets("exclude") {
        let ops = patterns
            .iter()
            .map(|p| FixOp::JsonAppend { path: key.clone(), value: Value::String(p.clone()) })
            .collect();
        return Fix::new(file, description, ops);
    }
    let mut list: Vec<String> = match &program.exclude {
        Some(inherited) => inherited.iter().map(|p| relative(&program.dir, Path::new(p))).collect(),
        None => {
            program.path_option("outDir").map(|o| relative(&program.dir, &o)).into_iter().collect()
        }
    };
    for p in patterns {
        if !list.contains(p) {
            list.push(p.clone());
        }
    }
    let value = Value::Array(list.into_iter().map(Value::String).collect());
    Fix::new(file, description, vec![FixOp::JsonSet { path: key, value }])
}

fn quoted(items: &[String]) -> String {
    items.iter().map(|i| format!("\"{i}\"")).collect::<Vec<_>>().join(", ")
}
