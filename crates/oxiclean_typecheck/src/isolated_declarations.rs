//! `isolated-declarations`: packages that could emit `.d.ts` files
//! without type-checking.
//!
//! With `isolatedDeclarations`, every export carries the types its `.d.ts`
//! needs, so `tsc`, oxc or `tsgo` can write declaration files one file at a
//! time, without checking the program, and `tsc -b` can start dependents
//! before their dependencies finish. Reported, as `code`:
//!
//! - `can-enable`: a program that emits declarations, whose exports all
//!   have the types already. Fixed by turning `isolatedDeclarations` on,
//!   which keeps them that way.
//! - `few-annotations`: one that emits declarations and is a few
//!   annotations away, with where each is needed.
//! - `slow-types`: a Deno package with `exports` (a JSR package) whose
//!   public files have exports without explicit types, which JSR calls
//!   slow types: it can't generate docs or `.d.ts` files for them, and
//!   type-checking its users is slower.

use std::{
    collections::{BTreeSet, VecDeque},
    fs,
    path::{Path, PathBuf},
};

use dashmap::DashMap;
use oxc_allocator::Allocator;
use oxc_isolated_declarations::{IsolatedDeclarations, IsolatedDeclarationsOptions};
use oxc_parser::Parser;
use oxc_span::SourceType;
use oxiclean_core::{
    CheckResult, Finding, FindingKind, Issue, Project, Related, Result,
    fix::Fix,
    jsonc_tree::{JsoncDoc, Segment},
    parse_imports, parse_jsonc, resolve,
};
use rayon::prelude::*;
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::{
    config::{Config, DEFAULT_THRESHOLD, ISOLATED_DECLARATIONS_RULE},
    tsconfig::{TsConfigFile, TsConfigs},
};

/// Programs at most this many annotations away are reported.
const NEAR_READY: usize = 10;

/// How many missing annotations a finding lists.
const SAMPLES: usize = 5;

/// One export without the types its declaration needs.
#[derive(Debug)]
struct Missing {
    file: PathBuf,
    line: u32,
    message: String,
}

/// Runs the `isolated-declarations` check with `cfg`.
///
/// # Errors
/// Fails if the root can't be found or read, or the config file is invalid.
pub fn run_isolated_declarations_check(cfg: &Config) -> Result<CheckResult> {
    info!("Starting {ISOLATED_DECLARATIONS_RULE} check");
    let workspace = cfg.common.workspace()?;
    let policy = workspace.policy(ISOLATED_DECLARATIONS_RULE, DEFAULT_THRESHOLD)?;
    let configs = TsConfigs::load(&workspace.project);
    let mut findings = programs(&configs);
    findings.extend(jsr_packages(&workspace.project));
    info!("{ISOLATED_DECLARATIONS_RULE} check complete. Found {} findings", findings.len());
    Ok(workspace.finish(&policy, findings, configs.programs().count()))
}

/// Whether `program` writes `.d.ts` files.
fn emits_declarations(program: &TsConfigFile) -> bool {
    (program.flag("declaration") == Some(true) || program.flag("composite") == Some(true))
        && program.flag("noEmit") != Some(true)
}

/// Whether `file` is TypeScript source, which declarations are made from.
fn is_source(file: &Path) -> bool {
    let name = file.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
    [".ts", ".tsx", ".mts", ".cts"].iter().any(|e| name.ends_with(e)) && !name.contains(".d.")
}

/// Findings for the programs that emit declarations.
fn programs(configs: &TsConfigs) -> Vec<Finding> {
    let mut findings = Vec::new();
    for program in configs.programs() {
        if program.root_files.is_empty()
            || program.partial
            || program.flag("isolatedDeclarations").is_some()
            || !emits_declarations(program)
        {
            continue;
        }
        let files: Vec<&PathBuf> = program.root_files.iter().filter(|f| is_source(f)).collect();
        if files.is_empty() {
            continue;
        }
        let missing: Vec<Missing> = files.par_iter().flat_map(|f| missing_types(f)).collect();
        let line = program.line_of(&["compilerOptions"]);
        let finding = if missing.is_empty() {
            let message = "emits declarations and its exports already have the types isolatedDeclarations needs; turning it on lets tools write .d.ts files without type-checking, and keeps it that way";
            let fix = Fix::json_set(
                program.path.clone(),
                &["compilerOptions", "isolatedDeclarations"],
                json!(true),
            );
            Issue::new("can-enable", "isolatedDeclarations", message, line).with_fix(fix)
        } else if missing.len() <= NEAR_READY {
            let message = format!(
                "emits declarations and is {} away from isolatedDeclarations, which lets tools write .d.ts files without type-checking",
                annotations(missing.len())
            );
            Issue::new("few-annotations", "isolatedDeclarations", message, line)
                .with_related(related(&missing))
        } else {
            continue;
        };
        let value = missing.len().max(1);
        findings.push(Finding::new(program.path.clone(), FindingKind::Issue(finding), value));
    }
    findings
}

/// Findings for Deno packages published to JSR with slow types.
fn jsr_packages(project: &Project) -> Vec<Finding> {
    let cache = DashMap::new();
    let mut findings = Vec::new();
    for (name, dir) in project.workspace_packages() {
        let Some((config, line, entries)) = jsr_entries(dir) else { continue };
        let files = public_files(project, &cache, &entries);
        let missing: Vec<Missing> = files.par_iter().flat_map(|f| missing_types(f)).collect();
        if missing.is_empty() {
            continue;
        }
        let message = format!(
            "{name} is published with {} that JSR calls slow types: exports without explicit types, so JSR can't generate its docs or .d.ts files, and its users type-check slower",
            annotations(missing.len()).replace("annotation", "missing annotation")
        );
        let issue = Issue::new("slow-types", name, message, line).with_related(related(&missing));
        findings.push(Finding::new(config, FindingKind::Issue(issue), missing.len()));
    }
    findings
}

/// The deno.json of the package in `dir`, the line of its `exports`, and
/// its `exports` files, if it has any.
fn jsr_entries(dir: &Path) -> Option<(PathBuf, u32, Vec<PathBuf>)> {
    let (config, text) = ["deno.json", "deno.jsonc"].iter().find_map(|n| {
        let p = dir.join(n);
        let text = fs::read_to_string(&p).ok()?;
        Some((p, text))
    })?;
    let deno = parse_jsonc(&text)?;
    let line = JsoncDoc::parse(text).map_or(0, |d| d.line_of(&[Segment::from("exports")]));
    let entries: Vec<PathBuf> = match deno.get("exports")? {
        Value::String(s) => vec![dir.join(s)],
        Value::Object(map) => map.values().filter_map(Value::as_str).map(|s| dir.join(s)).collect(),
        _ => return None,
    };
    let entries: Vec<PathBuf> =
        entries.into_iter().map(path_clean::clean).filter(|e| e.is_file()).collect();
    (!entries.is_empty()).then_some((config, line, entries))
}

/// `entries` and the local files they re-export or import, which JSR
/// checks too.
fn public_files(
    project: &Project,
    cache: &DashMap<(PathBuf, String), Option<PathBuf>>,
    entries: &[PathBuf],
) -> Vec<PathBuf> {
    let mut seen: BTreeSet<PathBuf> = entries.iter().cloned().collect();
    let mut queue: VecDeque<PathBuf> = entries.iter().cloned().collect();
    while let Some(f) = queue.pop_front() {
        let specs = match parse_imports(&f) {
            Ok(parsed) => parsed.specs,
            Err(e) => {
                warn!("Skipping {}: {e}", f.display());
                continue;
            }
        };
        for s in specs.iter().filter(|s| s.request.starts_with('.')) {
            if let Ok(Some(local)) = resolve(project, &f, &s.request, cache)
                && is_source(&local)
                && seen.insert(local.clone())
            {
                queue.push_back(local);
            }
        }
    }
    seen.into_iter().collect()
}

/// The exports of `file` without the types `isolatedDeclarations` needs.
fn missing_types(file: &Path) -> Vec<Missing> {
    let Ok(src) = fs::read_to_string(file) else { return Vec::new() };
    let Ok(source_type) = SourceType::from_path(file) else { return Vec::new() };
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, &src, source_type).parse();
    if !parsed.errors.is_empty() {
        return Vec::new();
    }
    let options = IsolatedDeclarationsOptions::default();
    let ret = IsolatedDeclarations::new(&allocator, options).build(&parsed.program);
    ret.errors
        .iter()
        .map(|e| {
            let offset =
                e.labels.as_ref().and_then(|l| l.first()).map_or(0, |l| l.offset()).min(src.len());
            let line = src.as_bytes()[..offset].iter().filter(|b| **b == b'\n').count() + 1;
            Missing {
                file: file.to_path_buf(),
                line: u32::try_from(line).unwrap_or(u32::MAX),
                message: e.message.to_string(),
            }
        })
        .collect()
}

fn related(missing: &[Missing]) -> Vec<Related> {
    missing
        .iter()
        .take(SAMPLES)
        .map(|m| Related::new(m.file.clone(), m.line, m.message.clone()))
        .collect()
}

/// `1 annotation` or `n annotations`.
fn annotations(n: usize) -> String {
    if n == 1 { "1 annotation".to_string() } else { format!("{n} annotations") }
}
