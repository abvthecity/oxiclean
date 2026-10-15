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
//!   public API has exports without explicit types, which JSR calls slow
//!   types: it can't generate docs or `.d.ts` files for them, and
//!   type-checking its users is slower.
//!
//! Exports are checked with oxc's isolated declarations, plus `satisfies`
//! on exported variables, which oxc accepts and `tsc` doesn't. Programs
//! with `allowJs`, which `tsc` won't combine with the option, or on a
//! TypeScript before 5.5, which doesn't know it, aren't reported, nor are
//! programs with a file oxc can't parse.

use std::{
    collections::{BTreeSet, HashMap},
    fs,
    path::{Path, PathBuf},
};

use dashmap::DashMap;
use oxc_allocator::Allocator;
use oxc_ast::ast::{Declaration, Expression, Statement};
use oxc_isolated_declarations::{IsolatedDeclarations, IsolatedDeclarationsOptions};
use oxc_parser::Parser;
use oxc_span::SourceType;
use oxiclean_core::{
    CheckResult, Finding, FindingKind, Issue, Project, Related, Result,
    fix::Fix,
    jsonc_tree::{JsoncDoc, Segment},
    parse_jsonc, resolve,
};
use rayon::prelude::*;
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::{
    config::{Config, DEFAULT_THRESHOLD, ISOLATED_DECLARATIONS_RULE},
    tsconfig::{TsConfigFile, TsConfigs},
    typescript::typescript_version,
};

/// Programs at most this many annotations away are reported.
const NEAR_READY: usize = 10;

/// How many missing annotations a finding lists.
const SAMPLES: usize = 5;

/// The first TypeScript version with `isolatedDeclarations`.
const FIRST_VERSION: (u64, u64) = (5, 5);

/// What `tsc` says when a variable needs a type.
const VARIABLE_NEEDS_TYPE: &str =
    "TS9010: Variable must have an explicit type annotation with --isolatedDeclarations.";

/// One export without the types its declaration needs.
#[derive(Debug, Clone)]
struct Missing {
    file: PathBuf,
    line: u32,
    message: String,
}

/// What one file needs and where it leads.
#[derive(Debug, Default)]
struct FileReport {
    missing: Vec<Missing>,
    /// Modules it re-exports from, as written.
    reexports: Vec<String>,
    /// Modules it imports from, as written.
    imports: Vec<String>,
}

/// Which local modules a walk follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Follow {
    /// Imports and re-exports: what a program checks.
    All,
    /// Re-exports only: a package's public API.
    ReExports,
}

/// Files read so far, `None` for those that couldn't be parsed.
type Reports = HashMap<PathBuf, Option<FileReport>>;

/// Runs the `isolated-declarations` check with `cfg`.
///
/// # Errors
/// Fails if the root can't be found or read, or the config file is invalid.
pub fn run_isolated_declarations_check(cfg: &Config) -> Result<CheckResult> {
    info!("Starting {ISOLATED_DECLARATIONS_RULE} check");
    let workspace = cfg.common.workspace()?;
    let policy = workspace.policy(ISOLATED_DECLARATIONS_RULE, DEFAULT_THRESHOLD)?;
    let project = &workspace.project;
    let configs = TsConfigs::load(project);
    let cache = DashMap::new();
    let mut reports = Reports::new();
    let mut findings = programs(project, &configs, &cache, &mut reports);
    findings.extend(jsr_packages(project, &cache, &mut reports));
    info!("{ISOLATED_DECLARATIONS_RULE} check complete. Found {} findings", findings.len());
    Ok(workspace.finish(&policy, findings, reports.len()))
}

/// Whether `program` makes `.d.ts` files, or sets `declaration` for another
/// tool to.
fn emits_declarations(program: &TsConfigFile) -> bool {
    program.flag("declaration") == Some(true) || program.flag("composite") == Some(true)
}

/// Whether `file` is TypeScript source, which declarations are made from.
fn is_source(file: &Path) -> bool {
    let name = file.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
    [".ts", ".tsx", ".mts", ".cts"].iter().any(|e| name.ends_with(e)) && !name.contains(".d.")
}

/// Findings for the programs that emit declarations.
fn programs(
    project: &Project,
    configs: &TsConfigs,
    cache: &DashMap<(PathBuf, String), Option<PathBuf>>,
    reports: &mut Reports,
) -> Vec<Finding> {
    let mut findings = Vec::new();
    for program in configs.programs() {
        let too_old = typescript_version(project, &program.dir).is_some_and(|v| v < FIRST_VERSION);
        if program.root_files.is_empty()
            || program.partial
            || program.flag("isolatedDeclarations").is_some()
            || program.flag("allowJs") == Some(true)
            || too_old
            || !emits_declarations(program)
        {
            continue;
        }
        let starts: Vec<PathBuf> =
            program.root_files.iter().filter(|f| is_source(f)).cloned().collect();
        if starts.is_empty() {
            continue;
        }
        let Some(missing) = walk(project, cache, reports, &starts, Follow::All) else {
            warn!("Skipping {}: a file it checks couldn't be parsed", program.path.display());
            continue;
        };
        let line = program.line_of(&["compilerOptions"]);
        let issue = if missing.is_empty() {
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
                count(missing.len(), "annotation")
            );
            Issue::new("few-annotations", "isolatedDeclarations", message, line)
                .with_related(related(&missing))
        } else {
            continue;
        };
        findings.push(Finding::new(program.path.clone(), FindingKind::Issue(issue), 1));
    }
    findings
}

/// Findings for Deno packages, the root one included, published to JSR
/// with slow types.
fn jsr_packages(
    project: &Project,
    cache: &DashMap<(PathBuf, String), Option<PathBuf>>,
    reports: &mut Reports,
) -> Vec<Finding> {
    let mut dirs: BTreeSet<&Path> =
        project.workspace_packages().into_iter().map(|(_, d)| d).collect();
    dirs.insert(&project.root);
    let mut findings = Vec::new();
    for dir in dirs {
        let Some(package) = jsr_package(dir) else { continue };
        let Some(missing) = walk(project, cache, reports, &package.entries, Follow::ReExports)
        else {
            warn!("Skipping {}: a public file couldn't be parsed", package.config.display());
            continue;
        };
        if missing.is_empty() {
            continue;
        }
        let message = format!(
            "{} is published with {} JSR calls slow types: exports without explicit types, so JSR can't generate its docs or .d.ts files, and its users type-check slower",
            package.name,
            count(missing.len(), "export")
        );
        let issue = Issue::new("slow-types", package.name, message, package.line)
            .with_related(related(&missing));
        findings.push(Finding::new(package.config, FindingKind::Issue(issue), 1));
    }
    findings
}

/// A Deno package with `exports`.
#[derive(Debug)]
struct JsrPackage {
    config: PathBuf,
    name: String,
    /// The line of `exports` in `config`.
    line: u32,
    /// Its TypeScript `exports` files.
    entries: Vec<PathBuf>,
}

/// The package whose deno.json is in `dir`, if it has `exports`.
fn jsr_package(dir: &Path) -> Option<JsrPackage> {
    let (config, text) = ["deno.json", "deno.jsonc"].iter().find_map(|n| {
        let p = dir.join(n);
        let text = fs::read_to_string(&p).ok()?;
        Some((p, text))
    })?;
    let Some(deno) = parse_jsonc(&text) else {
        warn!("Skipping {}: not valid JSON", config.display());
        return None;
    };
    let entries: Vec<PathBuf> = match deno.get("exports")? {
        Value::String(s) => vec![dir.join(s)],
        Value::Object(map) => map.values().filter_map(Value::as_str).map(|s| dir.join(s)).collect(),
        _ => return None,
    };
    let entries: Vec<PathBuf> = entries
        .into_iter()
        .map(path_clean::clean)
        .filter(|e| is_source(e) && e.is_file())
        .collect();
    if entries.is_empty() {
        return None;
    }
    let name = deno.get("name").and_then(Value::as_str).unwrap_or(".").to_string();
    let line = JsoncDoc::parse(text).map_or(0, |d| d.line_of(&[Segment::from("exports")]));
    Some(JsrPackage { config, name, line, entries })
}

/// The missing types in `starts` and the local TypeScript files they lead
/// to, or `None` if one couldn't be parsed.
fn walk(
    project: &Project,
    cache: &DashMap<(PathBuf, String), Option<PathBuf>>,
    reports: &mut Reports,
    starts: &[PathBuf],
    follow: Follow,
) -> Option<Vec<Missing>> {
    let mut seen: BTreeSet<PathBuf> = starts.iter().cloned().collect();
    let mut frontier: Vec<PathBuf> = starts.to_vec();
    while !frontier.is_empty() {
        let todo: Vec<&PathBuf> = frontier.iter().filter(|f| !reports.contains_key(*f)).collect();
        let read: Vec<(PathBuf, Option<FileReport>)> =
            todo.into_par_iter().map(|f| (f.clone(), analyze(f))).collect();
        reports.extend(read);
        let mut next = Vec::new();
        for f in &frontier {
            let report = reports.get(f)?.as_ref()?;
            let imports: &[String] = if follow == Follow::All { &report.imports } else { &[] };
            for request in report.reexports.iter().chain(imports).filter(|r| r.starts_with('.')) {
                if let Ok(Some(local)) = resolve(project, f, request, cache)
                    && is_source(&local)
                    && !local.components().any(|c| c.as_os_str() == "node_modules")
                    && seen.insert(local.clone())
                {
                    next.push(local);
                }
            }
        }
        frontier = next;
    }
    Some(
        seen.iter()
            .filter_map(|f| reports.get(f)?.as_ref())
            .flat_map(|r| r.missing.iter().cloned())
            .collect(),
    )
}

/// What `file` exports without types, and what it imports and
/// re-exports; `None` if it can't be read or parsed.
fn analyze(file: &Path) -> Option<FileReport> {
    let src = fs::read_to_string(file).ok()?;
    let source_type = SourceType::from_path(file).ok()?;
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, &src, source_type).parse();
    if !parsed.errors.is_empty() {
        return None;
    }
    let line_starts: Vec<usize> =
        std::iter::once(0).chain(src.match_indices('\n').map(|(i, _)| i + 1)).collect();
    let line = |offset: usize| {
        u32::try_from(line_starts.partition_point(|&s| s <= offset)).unwrap_or(u32::MAX)
    };
    let mut report = FileReport::default();
    let mut is_module = false;
    for statement in &parsed.program.body {
        match statement {
            Statement::ImportDeclaration(d) => {
                is_module = true;
                report.imports.push(d.source.value.to_string());
            }
            Statement::ExportAllDeclaration(d) => {
                is_module = true;
                report.reexports.push(d.source.value.to_string());
            }
            Statement::ExportNamedDeclaration(d) => {
                is_module = true;
                if let Some(source) = &d.source {
                    report.reexports.push(source.value.to_string());
                }
                // oxc accepts `export const a = { ... } satisfies T`; `tsc`
                // needs a type on `a`.
                if let Some(Declaration::VariableDeclaration(v)) = &d.declaration {
                    for declarator in &v.declarations {
                        let satisfies = declarator.init.as_ref().is_some_and(|i| {
                            matches!(i.without_parentheses(), Expression::TSSatisfiesExpression(_))
                        });
                        if satisfies && declarator.id.type_annotation.is_none() {
                            report.missing.push(Missing {
                                file: file.to_path_buf(),
                                line: line(declarator.span.start as usize),
                                message: VARIABLE_NEEDS_TYPE.to_string(),
                            });
                        }
                    }
                }
            }
            Statement::ExportDefaultDeclaration(_)
            | Statement::TSExportAssignment(_)
            | Statement::TSNamespaceExportDeclaration(_) => is_module = true,
            _ => {}
        }
    }
    // A script declares globals, which `tsc` doesn't ask types of.
    if !is_module {
        report.missing.clear();
        return Some(report);
    }
    let ret = IsolatedDeclarations::new(&allocator, IsolatedDeclarationsOptions::default())
        .build(&parsed.program);
    for e in &ret.errors {
        let offset = e.labels.as_ref().and_then(|l| l.first()).map_or(0, |l| l.offset());
        let line = line(offset);
        if !report.missing.iter().any(|m| m.line == line) {
            report.missing.push(Missing {
                file: file.to_path_buf(),
                line,
                message: e.message.to_string(),
            });
        }
    }
    report.missing.sort_by_key(|m| m.line);
    Some(report)
}

fn related(missing: &[Missing]) -> Vec<Related> {
    missing
        .iter()
        .take(SAMPLES)
        .map(|m| Related::new(m.file.clone(), m.line, m.message.clone()))
        .collect()
}

/// `1 <noun>` or `n <noun>s`.
fn count(n: usize, noun: &str) -> String {
    if n == 1 { format!("1 {noun}") } else { format!("{n} {noun}s") }
}
