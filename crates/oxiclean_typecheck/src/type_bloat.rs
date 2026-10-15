//! `type-bloat`: package imports that load huge declaration trees.
//!
//! `bloat` counts the modules an import pulls into a bundle. `tsc` follows
//! a different graph: the `.d.ts` files a package's types entry imports,
//! re-exports and references, which it parses and binds in full, with
//! `skipLibCheck` or without. Some packages put everything behind one
//! entry: `import { google } from "googleapis"` loads about 3.7 million
//! lines of declarations, about 10s of `tsc` time, where the per-API
//! package `@googleapis/drive` loads 25 thousand. Reported, as `code`:
//!
//! - `heavy-types`: an import of a package whose declarations, beyond what
//!   the program loads anyway (its `@types` packages), have at least the
//!   threshold's lines. The value is those lines.
//!
//! Packages are resolved the way `tsc` does for each program: `paths`,
//! then `node_modules` from the importing file up, the package before its
//! `@types` package, with `exports` (and the `types` condition) when the
//! program's module resolution reads it, else `typesVersions`, `types`,
//! `typings` and `main`. Each program's source is followed through its
//! local imports.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use dashmap::DashMap;
use oxc_allocator::Allocator;
use oxc_ast::ast::{
    ExportAllDeclaration, ExportNamedDeclaration, Expression, ImportDeclaration, ImportExpression,
    TSImportEqualsDeclaration, TSImportType, TSLiteral, TSModuleReference, TSType,
};
use oxc_ast_visit::{Visit, walk};
use oxc_parser::Parser;
use oxc_span::SourceType;
use oxiclean_core::{
    CheckResult, Finding, FindingKind, Issue, Project, Result, parse_imports, split_package_request,
};
use rayon::prelude::*;
use serde_json::Value;
use tracing::{debug, info, warn};

use crate::{
    ambient_types::{loads_all_types, visible_types},
    config::{Config, TYPE_BLOAT_DEFAULT_THRESHOLD, TYPE_BLOAT_RULE},
    declarations::{Mode, package_file, paths_target, resolve_types},
    tsconfig::{TsConfigFile, TsConfigs},
};

/// A module a declaration file loads.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Request {
    /// An import, re-export, `import()` type or `/// <reference types>`.
    Module(String),
    /// A `/// <reference path>`, relative to the file.
    Path(String),
}

/// What one declaration file costs and loads.
#[derive(Debug, Default)]
struct DeclFile {
    lines: usize,
    requests: Vec<Request>,
}

/// One import of a package from a program's source.
#[derive(Debug)]
struct PackageImport<'a> {
    file: &'a Path,
    request: &'a str,
    line: u32,
}

/// Runs the `type-bloat` check with `cfg`.
///
/// # Errors
/// Fails if the root can't be found or read, or the config file is invalid.
pub fn run_type_bloat_check(cfg: &Config) -> Result<CheckResult> {
    info!("Starting {TYPE_BLOAT_RULE} check");
    let workspace = cfg.common.workspace()?;
    let policy = workspace.policy(TYPE_BLOAT_RULE, TYPE_BLOAT_DEFAULT_THRESHOLD)?;
    let Some(threshold) = policy.min_threshold() else {
        info!("{TYPE_BLOAT_RULE} is disabled");
        return Ok(workspace.finish(&policy, Vec::new(), 0));
    };
    let project = &workspace.project;
    let configs = TsConfigs::load(project);
    let programs: Vec<&TsConfigFile> =
        configs.programs().filter(|p| !p.partial && !p.root_files.is_empty()).collect();
    let graph = DeclGraph::new(project);
    let sources = read_sources(project, &programs);

    // The largest cost of each import, over the programs that check it.
    let mut costs: BTreeMap<(PathBuf, String), (u32, usize, usize)> = BTreeMap::new();
    for program in &programs {
        let mode = Mode::of(program);
        let baseline = graph.closure(&mode, &ambient_entries(project, program, &mode));
        for import in program_imports(project, program, &mode, &sources) {
            let entry = paths_target(project, program, import.request)
                .or_else(|| resolve_types(project, &mode, import.file, import.request));
            let Some(entry) = entry.filter(|e| is_declaration(e)) else { continue };
            let reach = graph.closure(&mode, &[entry]);
            let (lines, files) = reach
                .iter()
                .filter(|f| !baseline.contains(*f))
                .fold((0, 0), |(l, n), f| (l + graph.lines(f), n + 1));
            if lines < threshold {
                continue;
            }
            let key = (import.file.to_path_buf(), import.request.to_string());
            let cost = costs.entry(key).or_insert((import.line, lines, files));
            if lines > cost.1 {
                *cost = (import.line, lines, files);
            }
        }
    }
    let findings = costs
        .into_iter()
        .map(|((file, request), (line, lines, files))| {
            let (name, _) = split_package_request(&request);
            let message = format!(
                "loads {lines} lines of declarations in {files} files that tsc parses and binds in every program that imports it; import what you use from a narrower entry, such as a subpath of {name} or a smaller package"
            );
            let issue = Issue::new("heavy-types", request.clone(), message, line);
            Finding::new(file, FindingKind::Issue(issue), lines)
        })
        .collect::<Vec<_>>();
    let analyzed = sources.len() + graph.files.len();
    info!(findings = findings.len(), "{TYPE_BLOAT_RULE} check complete");
    Ok(workspace.finish(&policy, findings, analyzed))
}

/// The imports of every program's source files, and of the local files
/// they import, read in parallel.
fn read_sources(
    project: &Project,
    programs: &[&TsConfigFile],
) -> HashMap<PathBuf, Vec<oxiclean_core::Specifier>> {
    let mut sources: HashMap<PathBuf, Vec<oxiclean_core::Specifier>> = HashMap::new();
    let mut seen: HashSet<PathBuf> = programs
        .iter()
        .flat_map(|p| p.root_files.iter())
        .filter(|f| is_source(f))
        .cloned()
        .collect();
    let mut frontier: Vec<PathBuf> = seen.iter().cloned().collect();
    let mode = Mode { exports: false, conditions: Vec::new() };
    while !frontier.is_empty() {
        let read: Vec<(PathBuf, Vec<oxiclean_core::Specifier>, Vec<PathBuf>)> = frontier
            .into_par_iter()
            .filter_map(|f| {
                let specs = match parse_imports(&f) {
                    Ok(parsed) => parsed.specs,
                    Err(e) => {
                        warn!("Skipping {}: {e}", f.display());
                        return None;
                    }
                };
                let local = specs
                    .iter()
                    .filter(|s| s.request.starts_with('.'))
                    .filter_map(|s| resolve_types(project, &mode, &f, &s.request))
                    .filter(|l| is_source(l) && !in_node_modules(l))
                    .collect();
                Some((f, specs, local))
            })
            .collect();
        frontier = Vec::new();
        for (f, specs, local) in read {
            frontier.extend(local.into_iter().filter(|l| seen.insert(l.clone())));
            sources.insert(f, specs);
        }
    }
    sources
}

/// The package imports in `program`'s source: its root files and the
/// local files they lead to.
fn program_imports<'a>(
    project: &Project,
    program: &TsConfigFile,
    mode: &Mode,
    sources: &'a HashMap<PathBuf, Vec<oxiclean_core::Specifier>>,
) -> Vec<PackageImport<'a>> {
    let mut seen: HashSet<&Path> = HashSet::new();
    let mut stack: Vec<&Path> = Vec::new();
    for f in program.root_files.iter().filter(|f| is_source(f)) {
        if let Some((path, _)) = sources.get_key_value(f)
            && seen.insert(path)
        {
            stack.push(path);
        }
    }
    let mut out = Vec::new();
    while let Some(file) = stack.pop() {
        let Some(specs) = sources.get(file) else { continue };
        for s in specs {
            if s.request.starts_with('.') || s.request.starts_with('/') {
                let next = resolve_types(project, mode, file, &s.request)
                    .and_then(|l| sources.get_key_value(&l).map(|(p, _)| p.as_path()));
                if let Some(next) = next
                    && seen.insert(next)
                {
                    stack.push(next);
                }
            } else if !oxiclean_core::is_builtin(&s.request) {
                out.push(PackageImport { file, request: &s.request, line: s.line });
            }
        }
    }
    out
}

/// The declaration files `program` loads whatever it imports: the
/// `@types` packages in its `types`, or every visible one.
fn ambient_entries(project: &Project, program: &TsConfigFile, mode: &Mode) -> Vec<PathBuf> {
    let dirs: Vec<&Path> =
        program.dir.ancestors().take_while(|a| a.starts_with(&project.root)).collect();
    let names: Vec<String> = if loads_all_types(project, program) {
        visible_types(&dirs, &project.root).into_iter().map(|(n, _)| n).collect()
    } else if let Some(Value::Array(types)) = program.options.get("types") {
        types.iter().filter_map(Value::as_str).map(str::to_string).collect()
    } else {
        Vec::new()
    };
    names
        .iter()
        .filter_map(|name| {
            dirs.iter().find_map(|d| {
                let modules = d.join("node_modules");
                package_file(project, mode, &modules.join("@types").join(name), "")
                    .or_else(|| package_file(project, mode, &modules.join(name), ""))
            })
        })
        .collect()
}

/// The declaration files packages load, read once and shared by every
/// program.
struct DeclGraph<'p> {
    project: &'p Project,
    files: DashMap<PathBuf, Arc<DeclFile>>,
    resolved: DashMap<(Mode, PathBuf, Request), Option<PathBuf>>,
    closures: DashMap<(Mode, PathBuf), Arc<HashSet<PathBuf>>>,
}

impl<'p> DeclGraph<'p> {
    fn new(project: &'p Project) -> Self {
        Self { project, files: DashMap::new(), resolved: DashMap::new(), closures: DashMap::new() }
    }

    fn lines(&self, file: &Path) -> usize {
        self.files.get(file).map_or(0, |f| f.lines)
    }

    /// The declaration files `entries` load, `entries` included.
    fn closure(&self, mode: &Mode, entries: &[PathBuf]) -> Arc<HashSet<PathBuf>> {
        if let [entry] = entries
            && let Some(done) = self.closures.get(&(mode.clone(), entry.clone()))
        {
            return Arc::clone(&done);
        }
        let mut seen: HashSet<PathBuf> = entries.iter().cloned().collect();
        let mut frontier: Vec<PathBuf> = entries.to_vec();
        while !frontier.is_empty() {
            let next: Vec<PathBuf> = frontier
                .par_iter()
                .flat_map_iter(|f| {
                    let file = self.read(f);
                    file.requests
                        .iter()
                        .filter_map(|r| self.resolve(mode, f, r))
                        .collect::<Vec<_>>()
                })
                .collect();
            frontier = next.into_iter().filter(|n| seen.insert(n.clone())).collect();
        }
        let seen = Arc::new(seen);
        if let [entry] = entries {
            self.closures.insert((mode.clone(), entry.clone()), Arc::clone(&seen));
        }
        seen
    }

    fn resolve(&self, mode: &Mode, from: &Path, request: &Request) -> Option<PathBuf> {
        let dir = from.parent().unwrap_or(from).to_path_buf();
        let key = (mode.clone(), dir, request.clone());
        if let Some(hit) = self.resolved.get(&key) {
            return hit.clone();
        }
        let found = match request {
            Request::Module(m) => resolve_types(self.project, mode, from, m),
            Request::Path(p) => {
                let target = key.1.join(p);
                target.is_file().then(|| path_clean::clean(target))
            }
        }
        .filter(|f| is_declaration(f));
        self.resolved.insert(key, found.clone());
        found
    }

    fn read(&self, file: &Path) -> Arc<DeclFile> {
        if let Some(hit) = self.files.get(file) {
            return Arc::clone(&hit);
        }
        let parsed = Arc::new(read_declarations(file));
        self.files.insert(file.to_path_buf(), Arc::clone(&parsed));
        parsed
    }
}

/// The lines of `file` and the modules it loads.
fn read_declarations(file: &Path) -> DeclFile {
    let src = match fs::read_to_string(file) {
        Ok(src) => src,
        Err(e) => {
            debug!("Skipping {}: {e}", file.display());
            return DeclFile::default();
        }
    };
    let lines = src.lines().count();
    let mut requests = references(&src);
    let source_type = SourceType::from_path(file).unwrap_or_else(|_| SourceType::d_ts());
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, &src, source_type).parse();
    if parsed.panicked {
        debug!("{} couldn't be parsed; counting its lines only", file.display());
    } else {
        let mut collector = Requests { requests: Vec::new() };
        collector.visit_program(&parsed.program);
        requests.extend(collector.requests);
    }
    DeclFile { lines, requests }
}

/// The `/// <reference path="…" />` and `/// <reference types="…" />`
/// directives at the top of `src`.
fn references(src: &str) -> Vec<Request> {
    let mut out = Vec::new();
    for line in src.lines().map(str::trim) {
        if line.is_empty() || line.starts_with("/*") || line.starts_with('*') {
            continue;
        }
        let Some(rest) = line.strip_prefix("///") else {
            if line.starts_with("//") {
                continue;
            }
            break;
        };
        let Some(rest) = rest.trim_start().strip_prefix("<reference") else { continue };
        for (attr, make) in [
            ("path=", Request::Path as fn(String) -> Request),
            ("types=", Request::Module as fn(String) -> Request),
        ] {
            let Some(at) = rest.find(attr) else { continue };
            let value = &rest[at + attr.len()..];
            let Some(quote) = value.chars().next().filter(|c| matches!(c, '"' | '\'')) else {
                continue;
            };
            if let Some(end) = value[1..].find(quote) {
                out.push(make(value[1..=end].to_string()));
            }
        }
    }
    out
}

struct Requests {
    requests: Vec<Request>,
}

impl Requests {
    fn push(&mut self, request: &str) {
        self.requests.push(Request::Module(request.to_string()));
    }
}

impl<'a> Visit<'a> for Requests {
    fn visit_import_declaration(&mut self, it: &ImportDeclaration<'a>) {
        self.push(&it.source.value);
    }

    fn visit_export_named_declaration(&mut self, it: &ExportNamedDeclaration<'a>) {
        if let Some(source) = &it.source {
            self.push(&source.value);
        }
        walk::walk_export_named_declaration(self, it);
    }

    fn visit_export_all_declaration(&mut self, it: &ExportAllDeclaration<'a>) {
        self.push(&it.source.value);
    }

    fn visit_ts_import_equals_declaration(&mut self, it: &TSImportEqualsDeclaration<'a>) {
        if let TSModuleReference::ExternalModuleReference(ext) = &it.module_reference {
            self.push(&ext.expression.value);
        }
    }

    fn visit_ts_import_type(&mut self, it: &TSImportType<'a>) {
        if let TSType::TSLiteralType(t) = &it.argument
            && let TSLiteral::StringLiteral(s) = &t.literal
        {
            self.push(&s.value);
        }
        walk::walk_ts_import_type(self, it);
    }

    fn visit_import_expression(&mut self, it: &ImportExpression<'a>) {
        if let Expression::StringLiteral(s) = &it.source {
            self.push(&s.value);
        }
        walk::walk_import_expression(self, it);
    }
}

/// Whether `file` is TypeScript or JavaScript source, not a declaration.
fn is_source(file: &Path) -> bool {
    let name = file.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
    [".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs"].iter().any(|e| name.ends_with(e))
        && !is_declaration(file)
}

/// Whether `file` is a declaration file, as `tsc` decides: `.d.ts`,
/// `.d.mts`, `.d.cts`, or any `.ts` file with `.d.` in its name.
fn is_declaration(file: &Path) -> bool {
    let name = file.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
    [".d.mts", ".d.cts"].iter().any(|e| name.ends_with(e))
        || (name.ends_with(".ts") && name.contains(".d."))
}

fn in_node_modules(file: &Path) -> bool {
    file.components().any(|c| c.as_os_str() == "node_modules")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_reference_directives() {
        let src = "// header\n/* licence */\n/// <reference types=\"node\" />\n/// <reference path='./globals.d.ts' />\nexport {};\n/// <reference types=\"late\" />";
        assert_eq!(
            references(src),
            [Request::Module("node".into()), Request::Path("./globals.d.ts".into())]
        );
    }
}
