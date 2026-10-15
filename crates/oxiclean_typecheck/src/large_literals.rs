//! `large-literals`: data that `tsc` infers types for.
//!
//! The checker builds a type for every value in an object or array
//! literal, and for every value of each `.json` file a program imports,
//! then normalizes and compares them. A few thousand values cost a
//! noticeable part of a second in every program that loads them, and more
//! in the editor. Reported, as `code`:
//!
//! - `json`: a `.json` file a program imports, with how many values it
//!   holds. A `<name>.d.json.ts` next to it, declaring the type, with
//!   `allowArbitraryExtensions`, makes `tsc` read that instead.
//! - `literal`: an object or array literal of data, without functions or
//!   classes, in a TypeScript (or `allowJs`) file. An annotation doesn't
//!   help, since `tsc` still checks each value against it; moving the data
//!   to a `.json` file with a declaration, or loading it at runtime, does.
//!
//! The value of each finding is the number of values: array elements and
//! object properties, counted through nested literals. Deno, which always
//! types the `.json` it imports, has its package directories checked too.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    path::{Path, PathBuf},
};

use dashmap::DashMap;
use ignore::WalkBuilder;
use oxc_allocator::Allocator;
use oxc_ast::ast::{
    ArrayExpressionElement, CallExpression, ExportDefaultDeclaration, Expression,
    ImportDeclaration, ImportExpression, ObjectPropertyKind, VariableDeclarator,
};
use oxc_ast_visit::{Visit, walk};
use oxc_parser::Parser;
use oxc_span::{GetSpan, SourceType};
use oxiclean_core::{CheckResult, Finding, FindingKind, Issue, Project, Related, Result, resolve};
use rayon::prelude::*;
use serde_json::Value;
use tracing::{info, warn};

use crate::{
    config::{Config, LARGE_LITERALS_DEFAULT_THRESHOLD, LARGE_LITERALS_RULE},
    tsconfig::TsConfigs,
};

/// How many importers a `json` finding lists.
const SAMPLES: usize = 5;

/// A literal in a source file.
#[derive(Debug, Clone)]
struct Literal {
    line: u32,
    /// The variable it initializes, `default` for a default export, or
    /// `literal`.
    name: String,
    values: usize,
    as_const: bool,
}

/// What one source file holds and imports.
#[derive(Debug, Default)]
struct FileReport {
    literals: Vec<Literal>,
    /// `.json` modules it imports, as written, with the line.
    json: Vec<(String, u32)>,
}

/// A set of files `tsc` (or `deno check`) checks together.
struct Program {
    files: Vec<PathBuf>,
    /// Whether it types the `.json` files it imports.
    json: bool,
    /// Whether `deno check` checks it, rather than `tsc`.
    deno: bool,
}

/// The programs that import one `.json` file.
#[derive(Debug, Default)]
struct JsonUse {
    importers: BTreeSet<(PathBuf, u32)>,
    programs: usize,
    deno: bool,
}

/// Runs the `large-literals` check with `cfg`.
///
/// # Errors
/// Fails if the root can't be found or read, or the config file is invalid.
pub fn run_large_literals_check(cfg: &Config) -> Result<CheckResult> {
    info!("Starting {LARGE_LITERALS_RULE} check");
    let workspace = cfg.common.workspace()?;
    let policy = workspace.policy(LARGE_LITERALS_RULE, LARGE_LITERALS_DEFAULT_THRESHOLD)?;
    let Some(threshold) = policy.min_threshold() else {
        info!("{LARGE_LITERALS_RULE} is disabled");
        return Ok(workspace.finish(&policy, Vec::new(), 0));
    };
    let project = &workspace.project;
    let configs = TsConfigs::load(project);
    let mut programs: Vec<Program> = configs
        .programs()
        .map(|p| Program {
            files: p.root_files.iter().filter(|f| is_source(f)).cloned().collect(),
            json: p.flag("resolveJsonModule") != Some(false),
            deno: false,
        })
        .collect();
    programs.extend(deno_programs(project));

    let files: BTreeSet<&PathBuf> = programs.iter().flat_map(|p| &p.files).collect();
    let reports: HashMap<&PathBuf, FileReport> =
        files.into_par_iter().filter_map(|f| Some((f, analyze(f, threshold)?))).collect();

    let deno: BTreeSet<&PathBuf> =
        programs.iter().filter(|p| p.deno).flat_map(|p| &p.files).collect();
    let mut findings = Vec::new();
    for (file, report) in &reports {
        let (checker, instead) = if deno.contains(file) {
            ("deno check", "Loading the data at runtime does")
        } else {
            (
                "tsc",
                "Moving the data to a .json file with a .d.json.ts declaration, or loading it at runtime, does",
            )
        };
        for literal in &report.literals {
            let mut message = format!(
                "{} values that {checker} infers types for in every program that checks this file; an annotation doesn't help, since each value is still checked against it. {instead}",
                literal.values
            );
            if literal.as_const {
                message.push_str("; `as const` makes a literal type for every value, too");
            }
            let issue = Issue::new("literal", literal.name.clone(), message, literal.line);
            findings.push(Finding::new((*file).clone(), FindingKind::Issue(issue), literal.values));
        }
    }
    findings.extend(json_findings(project, &programs, &reports, threshold));
    let analyzed = reports.len();
    info!("{LARGE_LITERALS_RULE} check complete. Found {} findings", findings.len());
    Ok(workspace.finish(&policy, findings, analyzed))
}

/// Findings for the `.json` files programs import, one per file.
fn json_findings(
    project: &Project,
    programs: &[Program],
    reports: &HashMap<&PathBuf, FileReport>,
    threshold: usize,
) -> Vec<Finding> {
    let cache = DashMap::new();
    // Each `.json` file, with its importers and how many programs load it.
    let mut imported: BTreeMap<PathBuf, JsonUse> = BTreeMap::new();
    for program in programs.iter().filter(|p| p.json) {
        let mut loaded = BTreeSet::new();
        for file in &program.files {
            let Some(report) = reports.get(file) else { continue };
            for (request, line) in &report.json {
                let Ok(Some(json)) = resolve(project, file, request, &cache) else { continue };
                if !is_json(&json) || has_declaration(&json) {
                    continue;
                }
                let uses = imported.entry(json.clone()).or_default();
                uses.importers.insert((file.clone(), *line));
                uses.deno |= program.deno;
                loaded.insert(json);
            }
        }
        for json in loaded {
            if let Some(uses) = imported.get_mut(&json) {
                uses.programs += 1;
            }
        }
    }
    let counted: Vec<(PathBuf, usize)> = imported
        .keys()
        .collect::<Vec<_>>()
        .into_par_iter()
        .filter_map(|json| Some((json.clone(), json_values(json)?)))
        .collect();
    let mut findings = Vec::new();
    for (json, values) in counted {
        if values < threshold {
            continue;
        }
        let Some(uses) = imported.get(&json) else { continue };
        let name = json.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let stem = name.strip_suffix(".json").unwrap_or(&name);
        let checker = if uses.deno { "deno check" } else { "tsc" };
        let instead = if uses.deno {
            "reading it at runtime, with Deno.readTextFile and JSON.parse, skips that".to_string()
        } else if json.components().any(|c| c.as_os_str() == "node_modules") {
            "it comes from a dependency, so reading it at runtime instead of importing it skips that".to_string()
        } else {
            format!(
                "a {stem}.d.json.ts next to it declaring its type, with allowArbitraryExtensions, lets tsc read that instead"
            )
        };
        let message = format!(
            "{values} values that {checker} infers types for in every program that imports it ({}); {instead}",
            count(uses.programs, "program")
        );
        let related = uses
            .importers
            .iter()
            .take(SAMPLES)
            .map(|(f, line)| Related::new(f.clone(), *line, format!("imports {name}")))
            .collect();
        let issue = Issue::new("json", name.clone(), message, 1).with_related(related);
        findings.push(Finding::new(json, FindingKind::Issue(issue), values));
    }
    findings
}

/// One program per Deno package, the root one included, with the
/// TypeScript files under it: `deno check` types the `.json` files they
/// import.
fn deno_programs(project: &Project) -> Vec<Program> {
    let mut dirs: BTreeSet<&Path> =
        project.workspace_packages().into_iter().map(|(_, d)| d).collect();
    dirs.insert(&project.root);
    let deno: Vec<&Path> = dirs
        .into_iter()
        .filter(|d| d.join("deno.json").is_file() || d.join("deno.jsonc").is_file())
        .collect();
    deno.iter()
        .map(|dir| {
            let walker = WalkBuilder::new(dir)
                .hidden(true)
                .git_ignore(true)
                .filter_entry(|e| e.file_name() != "node_modules")
                .build();
            let mut files: Vec<PathBuf> = walker
                .filter_map(std::result::Result::ok)
                .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
                .map(ignore::DirEntry::into_path)
                .filter(|p| is_typescript(p))
                // A nested Deno package is a program of its own.
                .filter(|p| {
                    !deno
                        .iter()
                        .any(|other| other != dir && other.starts_with(dir) && p.starts_with(other))
                })
                .collect();
            files.sort();
            Program { files, json: true, deno: true }
        })
        .collect()
}

/// Whether `file` is source `tsc` infers types in: not a declaration file.
fn is_source(file: &Path) -> bool {
    let name = file.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
    [".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs"].iter().any(|e| name.ends_with(e))
        && !name.contains(".d.")
}

/// Whether `file` is TypeScript source.
fn is_typescript(file: &Path) -> bool {
    let name = file.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
    [".ts", ".tsx", ".mts", ".cts"].iter().any(|e| name.ends_with(e)) && !name.contains(".d.")
}

fn is_json(file: &Path) -> bool {
    file.extension().is_some_and(|e| e == "json")
}

/// Whether `<name>.d.json.ts` declares `json`'s type, which `tsc` then
/// reads instead.
fn has_declaration(json: &Path) -> bool {
    let Some(name) = json.file_name().map(|n| n.to_string_lossy().into_owned()) else {
        return false;
    };
    let stem = name.strip_suffix(".json").unwrap_or(&name);
    json.with_file_name(format!("{stem}.d.json.ts")).is_file()
}

/// How many values `json` holds, or `None` if it isn't valid JSON.
fn json_values(json: &Path) -> Option<usize> {
    let text = fs::read_to_string(json).ok()?;
    match serde_json::from_str::<Value>(&text) {
        Ok(value) => Some(values_in(&value)),
        Err(e) => {
            warn!("Skipping {}: {e}", json.display());
            None
        }
    }
}

/// The values nested in `value`: elements and properties, counted
/// through nested arrays and objects.
fn values_in(value: &Value) -> usize {
    match value {
        Value::Array(a) => a.iter().map(|v| 1 + values_in(v)).sum(),
        Value::Object(o) => o.values().map(|v| 1 + values_in(v)).sum(),
        _ => 0,
    }
}

/// The literals in `file` with at least `threshold` values, and the
/// `.json` modules it imports; `None` if it can't be read or parsed.
fn analyze(file: &Path, threshold: usize) -> Option<FileReport> {
    let src = fs::read_to_string(file).ok()?;
    let source_type = SourceType::from_path(file).ok()?;
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, &src, source_type).parse();
    if parsed.panicked {
        warn!("Skipping {}: it couldn't be parsed", file.display());
        return None;
    }
    let line_starts: Vec<usize> =
        std::iter::once(0).chain(src.match_indices('\n').map(|(i, _)| i + 1)).collect();
    let mut collector = Collector { threshold, line_starts, report: FileReport::default() };
    collector.visit_program(&parsed.program);
    Some(collector.report)
}

struct Collector {
    threshold: usize,
    line_starts: Vec<usize>,
    report: FileReport,
}

impl Collector {
    fn line(&self, offset: u32) -> u32 {
        let at = self.line_starts.partition_point(|&s| s <= offset as usize);
        u32::try_from(at).unwrap_or(u32::MAX)
    }

    fn json(&mut self, request: &str, offset: u32) {
        if request.ends_with(".json") {
            let line = self.line(offset);
            self.report.json.push((request.to_string(), line));
        }
    }

    /// Records `expr` if it's a data literal with enough values. Returns
    /// whether it's a data literal, whose contents then needn't be
    /// visited: none of them holds more values than it does.
    fn literal(&mut self, expr: &Expression<'_>, name: &str) -> bool {
        let (inner, as_const) = unwrap(expr);
        if !matches!(inner, Expression::ArrayExpression(_) | Expression::ObjectExpression(_)) {
            return false;
        }
        // A literal with code in it is walked like any other code.
        let Some(values) = values_of(inner) else { return false };
        if values >= self.threshold {
            let line = self.line(expr.span().start);
            self.report.literals.push(Literal { line, name: name.to_string(), values, as_const });
        }
        true
    }
}

impl<'a> Visit<'a> for Collector {
    fn visit_variable_declarator(&mut self, it: &VariableDeclarator<'a>) {
        if let Some(init) = &it.init {
            let name = it.id.get_identifier_name().map_or("literal", |n| n.as_str()).to_string();
            if self.literal(init, &name) {
                return;
            }
        }
        walk::walk_variable_declarator(self, it);
    }

    fn visit_export_default_declaration(&mut self, it: &ExportDefaultDeclaration<'a>) {
        if let Some(expr) = it.declaration.as_expression()
            && self.literal(expr, "default")
        {
            return;
        }
        walk::walk_export_default_declaration(self, it);
    }

    fn visit_expression(&mut self, it: &Expression<'a>) {
        if !self.literal(it, "literal") {
            walk::walk_expression(self, it);
        }
    }

    fn visit_import_declaration(&mut self, it: &ImportDeclaration<'a>) {
        // `import type` loads the module's type all the same.
        self.json(&it.source.value, it.span.start);
    }

    fn visit_import_expression(&mut self, it: &ImportExpression<'a>) {
        if let Expression::StringLiteral(s) = &it.source {
            self.json(&s.value, it.span.start);
        }
        walk::walk_import_expression(self, it);
    }

    fn visit_call_expression(&mut self, it: &CallExpression<'a>) {
        if let Expression::Identifier(callee) = &it.callee
            && callee.name == "require"
            && let [arg] = it.arguments.as_slice()
            && let Some(Expression::StringLiteral(s)) = arg.as_expression()
        {
            self.json(&s.value, it.span.start);
        }
        walk::walk_call_expression(self, it);
    }
}

/// `expr` without parentheses, `satisfies` and `as const`, and whether it
/// had `as const`.
fn unwrap<'b, 'a>(expr: &'b Expression<'a>) -> (&'b Expression<'a>, bool) {
    let mut expr = expr.without_parentheses();
    let mut as_const = false;
    loop {
        match expr {
            Expression::TSSatisfiesExpression(e) => expr = e.expression.without_parentheses(),
            Expression::TSAsExpression(e) if e.type_annotation.is_const_type_reference() => {
                as_const = true;
                expr = e.expression.without_parentheses();
            }
            _ => return (expr, as_const),
        }
    }
}

/// The values nested in a literal: elements and properties, counted
/// through nested literals. `None` if it holds code, a function or a
/// class, which makes it more than data: moving it out isn't an option.
fn values_of(expr: &Expression<'_>) -> Option<usize> {
    let leaf = |e: &Expression<'_>| {
        let (inner, _) = unwrap(e);
        match inner {
            Expression::ArrayExpression(_) | Expression::ObjectExpression(_) => values_of(inner),
            Expression::FunctionExpression(_)
            | Expression::ArrowFunctionExpression(_)
            | Expression::ClassExpression(_) => None,
            _ => Some(0),
        }
    };
    match expr {
        Expression::ArrayExpression(a) => {
            a.elements
                .iter()
                .map(|e| match e {
                    ArrayExpressionElement::SpreadElement(_)
                    | ArrayExpressionElement::Elision(_) => Some(1),
                    e => e.as_expression().map_or(Some(0), leaf).map(|n| n + 1),
                })
                .sum()
        }
        Expression::ObjectExpression(o) => o
            .properties
            .iter()
            .map(|p| match p {
                ObjectPropertyKind::ObjectProperty(p) => leaf(&p.value).map(|n| n + 1),
                ObjectPropertyKind::SpreadProperty(_) => Some(1),
            })
            .sum(),
        _ => Some(0),
    }
}

/// `1 <noun>` or `n <noun>s`.
fn count(n: usize, noun: &str) -> String {
    if n == 1 { format!("1 {noun}") } else { format!("{n} {noun}s") }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn literals(src: &str, threshold: usize) -> Vec<(u32, String, usize, bool)> {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.ts");
        fs::write(&file, src).unwrap();
        analyze(&file, threshold)
            .unwrap()
            .literals
            .into_iter()
            .map(|l| (l.line, l.name, l.values, l.as_const))
            .collect()
    }

    #[test]
    fn counts_values_through_nested_literals() {
        assert_eq!(
            literals("export const a = [{ x: 1, y: [1, 2] }, 3] as const;", 1),
            [(1, "a".to_string(), 6, true)]
        );
        assert_eq!(
            literals("export default ({ a: { b: 1 } }) satisfies object;", 2),
            [(1, "default".to_string(), 2, false)]
        );
        assert_eq!(
            literals("const a = f([1, 2, 3]);\nconst b = [1];", 3),
            [(1, "literal".to_string(), 3, false)]
        );
    }

    #[test]
    fn collects_json_imports() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.ts");
        fs::write(
            &file,
            "import a from './a.json' with { type: 'json' };\nimport type B from './b.json';\nconst c = require('./c.json');\nconst d = await import('./d.json');\nimport e from './e.js';",
        )
        .unwrap();
        let report = analyze(&file, 1).unwrap();
        let requests: Vec<&str> = report.json.iter().map(|(r, _)| r.as_str()).collect();
        assert_eq!(requests, ["./a.json", "./b.json", "./c.json", "./d.json"]);
    }
}
