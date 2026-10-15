//! `large-literals`: data that `tsc` infers types for.
//!
//! The checker builds a type for every value in an object or array
//! literal, and for every value of each `.json` file a program imports,
//! then normalizes and compares them: from tens of milliseconds per few
//! thousand values of uniform data to several times that for mixed
//! shapes, in every program that loads them, and again in the editor.
//! Reported, as `code`:
//!
//! - `json`: a `.json` file a program imports, with how many values it
//!   holds. A `<name>.d.json.ts` next to it, declaring the type, with
//!   `allowArbitraryExtensions`, makes `tsc` read that instead (and stop
//!   copying the `.json` file to the output).
//! - `literal`: an object or array literal of plain data (strings,
//!   numbers, booleans, `null` and nested literals) in a TypeScript (or
//!   `allowJs`) file. An annotation doesn't help, since `tsc` still checks
//!   each value against it; moving the data to a `.json` file with a
//!   declaration, or loading it at runtime, does.
//!
//! The value of each finding is the number of values: array elements and
//! object properties, counted through nested literals. Each program's
//! files are followed through their local imports, as `tsc` does. Deno
//! projects, which `deno check` checks without a tsconfig and which always
//! type the `.json` they import, are found by their `deno.json`.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

use dashmap::DashMap;
use ignore::WalkBuilder;
use oxc_allocator::Allocator;
use oxc_ast::ast::{
    ArrayExpressionElement, CallExpression, ExportAllDeclaration, ExportDefaultDeclaration,
    ExportNamedDeclaration, Expression, ImportDeclaration, ImportExpression, ObjectPropertyKind,
    TSImportEqualsDeclaration, TSImportType, TSLiteral, TSModuleReference, TSType,
    VariableDeclarator,
};
use oxc_ast_visit::{Visit, walk};
use oxc_parser::Parser;
use oxc_span::{GetSpan, SourceType};
use oxiclean_core::{
    CheckResult, Finding, FindingKind, Issue, Project, Related, Result, Severity, Workspace,
    resolve,
};
use rayon::prelude::*;
use serde::de::{DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_json::Value;
use tracing::{debug, info, warn};

use crate::{
    config::{Config, LARGE_LITERALS_DEFAULT_THRESHOLD, LARGE_LITERALS_RULE},
    declarations::{in_node_modules, is_source, is_typescript},
    lines::LineIndex,
    tsconfig::{TsConfigFile, TsConfigs},
    typescript::typescript_version,
};

/// How many importers a `json` finding lists.
const SAMPLES: usize = 5;

/// Literals nested deeper than this aren't counted: no real data is, and
/// counting them would use up the stack.
const MAX_DEPTH: usize = 128;

type ResolveCache = DashMap<(PathBuf, String), Option<PathBuf>>;

/// A literal in a source file.
#[derive(Debug)]
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
    /// Other relative modules it imports, as written.
    local: Vec<String>,
}

/// A set of files `tsc` (or `deno check`) checks together.
#[derive(Debug)]
struct Program {
    /// The files it starts from.
    roots: Vec<PathBuf>,
    /// Whether it types the `.json` files it imports.
    json: bool,
    /// Whether `deno check` checks it, rather than `tsc`.
    deno: bool,
    /// Whether `tsc` writes JavaScript for it, copying the `.json` files
    /// it imports to the output.
    emits: bool,
}

/// The programs that import one `.json` file.
#[derive(Debug, Default)]
struct JsonUse {
    importers: BTreeSet<(PathBuf, u32)>,
    programs: usize,
    deno: bool,
    emits: bool,
}

/// Runs the `large-literals` check with `cfg`.
///
/// # Errors
/// Fails if the root can't be found or read, or the config file is invalid.
pub fn run_large_literals_check(cfg: &Config) -> Result<CheckResult> {
    let workspace = cfg.common.workspace()?;
    run_on(&workspace, &TsConfigs::load(&workspace.project))
}

/// Runs the check on `workspace`, whose tsconfig files are `configs`.
pub(crate) fn run_on(workspace: &Workspace, configs: &TsConfigs) -> Result<CheckResult> {
    info!("Starting {LARGE_LITERALS_RULE} check");
    let policy =
        workspace.policy(LARGE_LITERALS_RULE, LARGE_LITERALS_DEFAULT_THRESHOLD, Severity::Error)?;
    let Some(threshold) = policy.min_threshold() else {
        info!("{LARGE_LITERALS_RULE} is disabled");
        return Ok(workspace.finish(&policy, Vec::new(), 0));
    };
    let project = &workspace.project;
    let mut programs: Vec<Program> = configs
        .programs()
        .map(|p| Program {
            roots: p.root_files.iter().filter(|f| is_source(f)).cloned().collect(),
            json: types_json(p, typescript_version(project, &p.dir)),
            deno: false,
            emits: p.flag("noEmit") != Some(true) && p.flag("emitDeclarationOnly") != Some(true),
        })
        .collect();
    programs.extend(deno_programs(project));

    let cache = ResolveCache::new();
    let (reports, edges) = read_files(project, &cache, &programs, threshold);
    let files: Vec<Vec<&Path>> = programs.iter().map(|p| closure(&p.roots, &edges)).collect();

    let deno: HashSet<&Path> = programs
        .iter()
        .zip(&files)
        .filter(|(p, _)| p.deno)
        .flat_map(|(_, f)| f.iter().copied())
        .collect();
    let mut findings = Vec::new();
    for (file, report) in &reports {
        let (checker, instead) = if deno.contains(file.as_path()) {
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
            findings.push(Finding::new(file.clone(), FindingKind::Issue(issue), literal.values));
        }
    }
    findings.extend(json_findings(project, &cache, &programs, &files, &reports, threshold));
    let analyzed = reports.len();
    info!(findings = findings.len(), "{LARGE_LITERALS_RULE} check complete");
    Ok(workspace.finish(&policy, findings, analyzed))
}

/// Reads every program's files, and the local files they import, in
/// parallel. Returns what each holds, and the local files each imports.
fn read_files(
    project: &Project,
    cache: &ResolveCache,
    programs: &[Program],
    threshold: usize,
) -> (HashMap<PathBuf, FileReport>, HashMap<PathBuf, Vec<PathBuf>>) {
    let mut reports: HashMap<PathBuf, FileReport> = HashMap::new();
    let mut edges: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
    let mut seen: HashSet<PathBuf> =
        programs.iter().flat_map(|p| p.roots.iter().cloned()).collect();
    let mut frontier: Vec<PathBuf> = seen.iter().cloned().collect();
    while !frontier.is_empty() {
        let read: Vec<(PathBuf, FileReport, Vec<PathBuf>)> = frontier
            .into_par_iter()
            .filter_map(|f| {
                let report = analyze(&f, threshold)?;
                let local = report
                    .local
                    .iter()
                    .filter_map(|r| resolve(project, &f, r, cache).ok().flatten())
                    .filter(|l| is_source(l) && !in_node_modules(l))
                    .collect();
                Some((f, report, local))
            })
            .collect();
        frontier = Vec::new();
        for (f, report, local) in read {
            frontier.extend(local.iter().filter(|l| seen.insert((*l).clone())).cloned());
            reports.insert(f.clone(), report);
            edges.insert(f, local);
        }
    }
    (reports, edges)
}

/// The files `roots` lead to through local imports, `roots` included.
fn closure<'a>(roots: &'a [PathBuf], edges: &'a HashMap<PathBuf, Vec<PathBuf>>) -> Vec<&'a Path> {
    let mut seen: HashSet<&Path> = roots.iter().map(PathBuf::as_path).collect();
    let mut stack: Vec<&Path> = seen.iter().copied().collect();
    while let Some(f) = stack.pop() {
        for next in edges.get(f).into_iter().flatten() {
            if seen.insert(next) {
                stack.push(next);
            }
        }
    }
    let mut out: Vec<&Path> = seen.into_iter().collect();
    out.sort();
    out
}

/// Findings for the `.json` files programs import, one per file.
fn json_findings(
    project: &Project,
    cache: &ResolveCache,
    programs: &[Program],
    files: &[Vec<&Path>],
    reports: &HashMap<PathBuf, FileReport>,
    threshold: usize,
) -> Vec<Finding> {
    // Each `.json` file, with its importers and how many programs load it.
    let mut imported: BTreeMap<PathBuf, JsonUse> = BTreeMap::new();
    for (program, files) in programs.iter().zip(files).filter(|(p, _)| p.json) {
        let mut loaded = BTreeSet::new();
        for &file in files {
            let Some(report) = reports.get(file) else { continue };
            for (request, line) in &report.json {
                let json = match resolve(project, file, request, cache) {
                    Ok(Some(json)) => json,
                    Ok(None) => {
                        debug!("{request} from {} doesn't resolve", file.display());
                        continue;
                    }
                    Err(e) => {
                        debug!("{request} from {} doesn't resolve: {e}", file.display());
                        continue;
                    }
                };
                if !is_json(&json) || has_declaration(&json) {
                    continue;
                }
                let uses = imported.entry(json.clone()).or_default();
                uses.importers.insert((file.to_path_buf(), *line));
                uses.deno |= program.deno;
                uses.emits |= program.emits;
                loaded.insert(json);
            }
        }
        for json in loaded {
            if let Some(uses) = imported.get_mut(&json) {
                uses.programs += 1;
            }
        }
    }
    let counted: Vec<(&PathBuf, &JsonUse, usize)> = imported
        .par_iter()
        .filter_map(|(json, uses)| Some((json, uses, json_values(json)?)))
        .filter(|(_, _, values)| *values >= threshold)
        .collect();
    let mut findings = Vec::new();
    for (json, uses, values) in counted {
        let name = json.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let stem = name.strip_suffix(".json").unwrap_or(&name);
        let checker = if uses.deno { "deno check" } else { "tsc" };
        let instead = if uses.deno {
            format!(
                "reading it at runtime, with JSON.parse(await Deno.readTextFile(new URL(\"./{name}\", import.meta.url))), skips that"
            )
        } else if in_node_modules(json) {
            "it comes from a dependency, so reading it at runtime instead of importing it skips that".to_string()
        } else if uses.emits {
            format!(
                "a {stem}.d.json.ts next to it declaring its type, with allowArbitraryExtensions, lets tsc read that instead, though tsc then stops copying {name} to the output, so the build has to copy it"
            )
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
        findings.push(Finding::new(json.clone(), FindingKind::Issue(issue), values));
    }
    findings
}

/// One program per directory with a `deno.json` (or `deno.jsonc`) that
/// git doesn't ignore, with the TypeScript files under it: `deno check`
/// types the `.json` files they import.
fn deno_programs(project: &Project) -> Vec<Program> {
    let walker = WalkBuilder::new(&project.root)
        .hidden(true)
        .git_ignore(true)
        .filter_entry(|e| e.file_name() != "node_modules")
        .build();
    let mut deno: Vec<PathBuf> = Vec::new();
    let mut typescript: Vec<PathBuf> = Vec::new();
    for entry in walker.filter_map(std::result::Result::ok) {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.into_path();
        let name = path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
        if name == "deno.json" || name == "deno.jsonc" {
            deno.extend(path.parent().map(Path::to_path_buf));
        } else if is_typescript(&path) {
            typescript.push(path);
        }
    }
    deno.sort();
    deno.dedup();
    let mut programs: Vec<Program> = deno
        .iter()
        .map(|_| Program { roots: Vec::new(), json: true, deno: true, emits: false })
        .collect();
    for file in typescript {
        // The nearest Deno project above the file checks it.
        let nearest = deno
            .iter()
            .enumerate()
            .filter(|(_, d)| file.starts_with(d))
            .max_by_key(|(_, d)| d.as_os_str().len());
        if let Some((i, _)) = nearest {
            programs[i].roots.push(file);
        }
    }
    programs.retain(|p| !p.roots.is_empty());
    programs
}

/// Whether `program` types the `.json` files it imports: with
/// `resolveJsonModule`, which defaults to on when modules resolve the
/// `bundler` or `nodenext` way. Unset, `module` decides: `preserve` and
/// `nodenext` (and, from TypeScript 6, `commonjs` and the ES targets)
/// resolve like that.
fn types_json(program: &TsConfigFile, version: Option<(u64, u64)>) -> bool {
    if let Some(on) = program.flag("resolveJsonModule") {
        return on;
    }
    let option = |k: &str| program.options.get(k).and_then(Value::as_str).map(str::to_lowercase);
    let ts6 = version.is_some_and(|v| v >= (6, 0));
    match option("moduleResolution").as_deref() {
        Some(r) => matches!(r, "bundler" | "nodenext"),
        None => match option("module").as_deref() {
            Some("preserve" | "nodenext") => true,
            Some("node16" | "node18" | "node20") => false,
            Some("commonjs" | "none" | "amd" | "umd" | "system") => ts6,
            Some(m) => ts6 && m.starts_with("es"),
            None => ts6,
        },
    }
}

/// Whether `file` is JavaScript, where `tsc` follows `require` calls.
fn is_javascript(file: &Path) -> bool {
    file.extension().is_some_and(|e| ["js", "jsx", "mjs", "cjs"].iter().any(|j| e == *j))
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
/// Counted as it's parsed, without building the document.
fn json_values(json: &Path) -> Option<usize> {
    let bytes = match fs::read(json) {
        Ok(b) => b,
        Err(e) => {
            debug!("Skipping {}: {e}", json.display());
            return None;
        }
    };
    let mut de = serde_json::Deserializer::from_slice(&bytes);
    match Count.deserialize(&mut de).and_then(|n| de.end().map(|()| n)) {
        Ok(n) => Some(n),
        Err(e) => {
            warn!("Skipping {}: {e}", json.display());
            None
        }
    }
}

/// Counts the values nested in a JSON value: elements and properties,
/// through nested arrays and objects.
struct Count;

impl<'de> DeserializeSeed<'de> for Count {
    type Value = usize;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<usize, D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Count {
    type Value = usize;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_bool<E>(self, _: bool) -> Result<usize, E> {
        Ok(0)
    }

    fn visit_i64<E>(self, _: i64) -> Result<usize, E> {
        Ok(0)
    }

    fn visit_u64<E>(self, _: u64) -> Result<usize, E> {
        Ok(0)
    }

    fn visit_f64<E>(self, _: f64) -> Result<usize, E> {
        Ok(0)
    }

    fn visit_str<E>(self, _: &str) -> Result<usize, E> {
        Ok(0)
    }

    fn visit_unit<E>(self) -> Result<usize, E> {
        Ok(0)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<usize, A::Error> {
        let mut n = 0;
        while let Some(inner) = seq.next_element_seed(Count)? {
            n += 1 + inner;
        }
        Ok(n)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<usize, A::Error> {
        let mut n = 0;
        while map.next_key::<IgnoredAny>()?.is_some() {
            n += 1 + map.next_value_seed(Count)?;
        }
        Ok(n)
    }
}

/// The literals in `file` with at least `threshold` values, and the
/// modules it imports; `None` if it can't be read or parsed.
fn analyze(file: &Path, threshold: usize) -> Option<FileReport> {
    let src = match fs::read_to_string(file) {
        Ok(src) => src,
        Err(e) => {
            debug!("Skipping {}: {e}", file.display());
            return None;
        }
    };
    let source_type = SourceType::from_path(file).ok()?;
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, &src, source_type).parse();
    if parsed.panicked {
        warn!("Skipping {}: it couldn't be parsed", file.display());
        return None;
    }
    let lines = LineIndex::new(&src);
    let mut collector = Collector {
        threshold,
        lines,
        report: FileReport::default(),
        depth: 0,
        javascript: is_javascript(file),
    };
    collector.visit_program(&parsed.program);
    Some(collector.report)
}

struct Collector {
    threshold: usize,
    lines: LineIndex,
    report: FileReport,
    /// How many data literals the visit is inside: the outermost one is
    /// the one counted.
    depth: usize,
    /// Whether the file is JavaScript, where `tsc` follows `require`.
    javascript: bool,
}

/// What a literal holds.
enum Contents {
    /// Plain data, with this many values.
    Data(usize),
    /// Code, such as a call, a variable or a function: the literal isn't
    /// only data, though what's in it may be.
    Code,
    /// Literals nested too deep to count.
    TooDeep,
}

impl Collector {
    fn line(&self, offset: u32) -> u32 {
        self.lines.line(offset as usize)
    }

    fn import(&mut self, request: &str, offset: u32) {
        if request.ends_with(".json") {
            let line = self.line(offset);
            self.report.json.push((request.to_string(), line));
        } else if request.starts_with('.') {
            self.report.local.push(request.to_string());
        }
    }

    /// If `expr` is an outermost data literal, records it when it has
    /// enough values and visits what's in it without counting its nested
    /// literals again. Returns whether it was one.
    fn data_literal(&mut self, expr: &Expression<'_>, name: &str) -> bool {
        if self.depth > 0 {
            return false;
        }
        let (inner, as_const) = strip_wrappers(expr);
        if !matches!(inner, Expression::ArrayExpression(_) | Expression::ObjectExpression(_)) {
            return false;
        }
        match contents(inner, 0) {
            // Walked like any other code, so the data in it is found.
            Contents::Code => false,
            Contents::TooDeep => true,
            Contents::Data(values) => {
                if values >= self.threshold {
                    let line = self.line(expr.span().start);
                    let name = name.to_string();
                    self.report.literals.push(Literal { line, name, values, as_const });
                }
                self.depth += 1;
                walk::walk_expression(self, expr);
                self.depth -= 1;
                true
            }
        }
    }
}

impl<'a> Visit<'a> for Collector {
    fn visit_variable_declarator(&mut self, it: &VariableDeclarator<'a>) {
        if let Some(init) = &it.init {
            let name = it.id.get_identifier_name().map_or("literal", |n| n.as_str()).to_string();
            if self.data_literal(init, &name) {
                return;
            }
        }
        walk::walk_variable_declarator(self, it);
    }

    fn visit_export_default_declaration(&mut self, it: &ExportDefaultDeclaration<'a>) {
        if let Some(expr) = it.declaration.as_expression()
            && self.data_literal(expr, "default")
        {
            return;
        }
        walk::walk_export_default_declaration(self, it);
    }

    fn visit_expression(&mut self, it: &Expression<'a>) {
        if !self.data_literal(it, "literal") {
            walk::walk_expression(self, it);
        }
    }

    fn visit_import_declaration(&mut self, it: &ImportDeclaration<'a>) {
        // `import type` loads the module's type all the same.
        self.import(&it.source.value, it.span.start);
    }

    fn visit_export_named_declaration(&mut self, it: &ExportNamedDeclaration<'a>) {
        if let Some(source) = &it.source {
            self.import(&source.value, it.span.start);
        }
        walk::walk_export_named_declaration(self, it);
    }

    fn visit_export_all_declaration(&mut self, it: &ExportAllDeclaration<'a>) {
        self.import(&it.source.value, it.span.start);
    }

    fn visit_ts_import_equals_declaration(&mut self, it: &TSImportEqualsDeclaration<'a>) {
        if let TSModuleReference::ExternalModuleReference(ext) = &it.module_reference {
            self.import(&ext.expression.value, it.span.start);
        }
    }

    fn visit_ts_import_type(&mut self, it: &TSImportType<'a>) {
        if let TSType::TSLiteralType(t) = &it.argument
            && let TSLiteral::StringLiteral(s) = &t.literal
        {
            self.import(&s.value, it.span.start);
        }
        walk::walk_ts_import_type(self, it);
    }

    fn visit_import_expression(&mut self, it: &ImportExpression<'a>) {
        if let Expression::StringLiteral(s) = &it.source {
            self.import(&s.value, it.span.start);
        }
        walk::walk_import_expression(self, it);
    }

    fn visit_call_expression(&mut self, it: &CallExpression<'a>) {
        // In TypeScript, `tsc` only follows `import x = require()`.
        if self.javascript
            && let Expression::Identifier(callee) = &it.callee
            && callee.name == "require"
            && let [arg] = it.arguments.as_slice()
            && let Some(Expression::StringLiteral(s)) = arg.as_expression()
        {
            self.import(&s.value, it.span.start);
        }
        walk::walk_call_expression(self, it);
    }
}

/// `expr` without parentheses, `satisfies`, `as`, `<T>` and `!`, and
/// whether it had `as const`.
fn strip_wrappers<'b, 'a>(expr: &'b Expression<'a>) -> (&'b Expression<'a>, bool) {
    let mut expr = expr.without_parentheses();
    let mut as_const = false;
    loop {
        expr = match expr {
            Expression::TSSatisfiesExpression(e) => &e.expression,
            Expression::TSAsExpression(e) => {
                as_const |= e.type_annotation.is_const_type_reference();
                &e.expression
            }
            Expression::TSTypeAssertion(e) => {
                as_const |= e.type_annotation.is_const_type_reference();
                &e.expression
            }
            Expression::TSNonNullExpression(e) => &e.expression,
            _ => return (expr, as_const),
        }
        .without_parentheses();
    }
}

/// What the literal `expr`, `depth` literals deep, holds: elements and
/// properties, counted through nested literals, if all of it is data.
fn contents(expr: &Expression<'_>, depth: usize) -> Contents {
    if depth > MAX_DEPTH {
        return Contents::TooDeep;
    }
    let value = |e: &Expression<'_>| {
        let (inner, _) = strip_wrappers(e);
        match inner {
            Expression::ArrayExpression(_) | Expression::ObjectExpression(_) => {
                contents(inner, depth + 1)
            }
            Expression::StringLiteral(_)
            | Expression::NumericLiteral(_)
            | Expression::BooleanLiteral(_)
            | Expression::NullLiteral(_)
            | Expression::BigIntLiteral(_) => Contents::Data(0),
            Expression::TemplateLiteral(t) if t.expressions.is_empty() => Contents::Data(0),
            Expression::UnaryExpression(u)
                if matches!(u.argument, Expression::NumericLiteral(_)) =>
            {
                Contents::Data(0)
            }
            _ => Contents::Code,
        }
    };
    let mut total = 0;
    let mut add = |c: Contents| match c {
        Contents::Data(n) => {
            total += 1 + n;
            None
        }
        other => Some(other),
    };
    match expr {
        Expression::ArrayExpression(a) => {
            for e in &a.elements {
                let c = match e {
                    ArrayExpressionElement::SpreadElement(_) => Contents::Code,
                    ArrayExpressionElement::Elision(_) => Contents::Data(0),
                    e => e.as_expression().map_or(Contents::Code, value),
                };
                if let Some(stop) = add(c) {
                    return stop;
                }
            }
        }
        Expression::ObjectExpression(o) => {
            for p in &o.properties {
                let c = match p {
                    ObjectPropertyKind::ObjectProperty(p) if !p.computed && !p.method => {
                        value(&p.value)
                    }
                    _ => Contents::Code,
                };
                if let Some(stop) = add(c) {
                    return stop;
                }
            }
        }
        _ => return Contents::Code,
    }
    Contents::Data(total)
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
            literals("export const a = [{ x: 1, y: [-1, `2`] }, 'a', null] as const;", 1),
            [(1, "a".to_string(), 7, true)]
        );
        assert_eq!(
            literals("export default ({ a: { b: 1 } }) satisfies object;", 2),
            [(1, "default".to_string(), 2, false)]
        );
        assert_eq!(
            literals("const a = f([1, 2, 3]);\nconst b = [1];", 3),
            [(1, "literal".to_string(), 3, false)]
        );
        // Assertions keep the variable's name.
        assert_eq!(
            literals("const rows = <Row[]>([{ a: 1 }, { a: 2 }] as Row[])!;", 4),
            [(1, "rows".to_string(), 4, false)]
        );
    }

    #[test]
    fn code_is_not_data() {
        // Functions, variables, calls and spreads make a literal code, but
        // the data in it is still found.
        assert_eq!(
            literals(
                "const c = { f() {}, d: [1, 2, 3], g: () => [4, 5, 6] };\nconst routes = [{ path: '/', component: Home }];\nconst e = { en: Object.freeze([1, 2, 3]) };\nconst s = [...[1, 2, 3]];",
                3
            ),
            [
                (1, "literal".to_string(), 3, false),
                (1, "literal".to_string(), 3, false),
                (3, "literal".to_string(), 3, false),
                (4, "literal".to_string(), 3, false),
            ]
        );
    }

    #[test]
    fn stops_at_deep_nesting() {
        let deep = format!("const a = {}1{};", "[".repeat(10_000), "]".repeat(10_000));
        let handle = std::thread::Builder::new()
            .stack_size(64 * 1024 * 1024)
            .spawn(move || literals(&deep, 1))
            .unwrap();
        assert!(handle.join().unwrap().is_empty());
    }

    #[test]
    fn knows_declaration_files() {
        use crate::declarations::is_declaration;
        // `tsc` reads `foo.d.utils.ts` as a declaration file too.
        for name in ["a.d.ts", "a.d.mts", "a.d.cts", "a.d.json.ts", "foo.d.utils.ts"] {
            assert!(is_declaration(Path::new(name)), "{name}");
        }
        for name in ["a.ts", "a.d.b.tsx", "d.ts", "a.d.js"] {
            assert!(!is_declaration(Path::new(name)), "{name}");
        }
    }

    fn imports(name: &str, src: &str) -> (Vec<String>, Vec<String>) {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(name);
        fs::write(&file, src).unwrap();
        let report = analyze(&file, 1).unwrap();
        (report.json.into_iter().map(|(r, _)| r).collect(), report.local)
    }

    #[test]
    fn collects_imports() {
        let (json, local) = imports(
            "a.ts",
            "import a from './a.json' with { type: 'json' };\nimport type B from './b.json';\nconst c = require('./c.json');\nconst d = await import('./d.json');\nimport e from './e.js';\nexport * from './f.json';\nexport { default as g } from './g.json';\nimport h = require('./h.json');\ntype I = typeof import('./i.json');\nexport default { fr: await import('./k.json') };\nimport 'pkg';",
        );
        assert_eq!(
            json,
            [
                "./a.json", "./b.json", "./d.json", "./f.json", "./g.json", "./h.json", "./i.json",
                "./k.json"
            ]
        );
        assert_eq!(local, ["./e.js"]);
        // `tsc` follows `require` in JavaScript only.
        let (json, _) = imports("a.js", "const m = { en: require('./en.json') };");
        assert_eq!(json, ["./en.json"]);
    }
}
