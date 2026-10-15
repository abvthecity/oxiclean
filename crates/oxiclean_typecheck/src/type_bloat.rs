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
//!   the program loads anyway, have at least the threshold's lines. The
//!   value is those lines. What the program loads anyway is its `@types`
//!   packages, its own declaration files, such as `env.d.ts`, and the JSX
//!   runtime under `react-jsx`. Declarations two imports share count for
//!   both.
//!
//! Packages are resolved the way `tsc` does for each program: `paths`,
//! then `node_modules` from the importing file up, the package before its
//! `@types` package, with `exports` when the program's module resolution
//! reads it (its conditions matched by the importing file's format under
//! `node16`), else `typesVersions`, `types`, `typings` and `main`. Files
//! are read once by their real path and their imports looked up from
//! there, as with pnpm's links. Each program's source is followed through
//! its local imports; a `declare module` block loads nothing.
//!
//! Imports inside declaration files are resolved without the program's
//! `paths`, which `tsc` applies there too; packages rarely rely on them.

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
    CheckResult, Finding, FindingKind, Issue, Project, Result, SpecKind, parse_imports,
    split_package_request,
};
use rayon::prelude::*;
use serde_json::Value;
use tracing::{debug, info, warn};

use crate::{
    ambient_types::{loads_all_types, visible_types},
    config::{Config, TYPE_BLOAT_DEFAULT_THRESHOLD, TYPE_BLOAT_RULE},
    declarations::{
        Format, Mode, in_node_modules, is_declaration, is_relative, is_source, package_file,
        paths_target, resolve_file_only, resolve_types, types_package_name,
    },
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

/// What an import costs: its line, and the lines and files of declarations
/// it adds.
#[derive(Debug, Clone, Copy)]
struct Cost {
    line: u32,
    lines: usize,
    files: usize,
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
    let graph = DeclGraph::new(project, &programs);
    let sources = read_sources(project, &programs);

    // The largest cost of each import, over the programs that check it.
    let mut costs: BTreeMap<(PathBuf, String), Cost> = BTreeMap::new();
    for program in &programs {
        let mode = Mode::of(project, program);
        let id = graph.mode_id(&mode);
        let baseline = graph.closure(id, &baseline_entries(project, program, &mode));
        for import in program_imports(project, program, &mode, &sources) {
            let format = mode.format(project, import.file);
            let entry = paths_target(project, program, import.request)
                .or_else(|| resolve_types(project, &mode, format, import.file, import.request));
            let Some(entry) = entry.filter(|e| is_declaration(e)) else { continue };
            let entry = fs::canonicalize(&entry).unwrap_or(entry);
            let reach = graph.closure(id, &[entry]);
            let (lines, files) = reach
                .iter()
                .filter(|f| !baseline.contains(*f))
                .fold((0, 0), |(l, n), f| (l + graph.lines(f), n + 1));
            if lines < threshold {
                continue;
            }
            let key = (import.file.to_path_buf(), import.request.to_string());
            let found = Cost { line: import.line, lines, files };
            let cost = costs.entry(key).or_insert(found);
            if lines > cost.lines {
                *cost = found;
            }
        }
    }
    let findings = costs
        .into_iter()
        .map(|((file, request), Cost { line, lines, files })| {
            let (name, sub) = split_package_request(&request);
            let advice = if sub.is_empty() {
                format!("import what you use from a narrower entry, such as a subpath of {name}, or from a smaller package")
            } else {
                "import from a smaller package, or from fewer programs".to_string()
            };
            let message = format!(
                "'{request}' loads {lines} lines of declarations in {files} files that tsc parses and binds in every program that imports it; {advice}"
            );
            let issue = Issue::new("heavy-types", request, message, line);
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
    let mode = Mode::local();
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
                    .filter(|s| is_relative(&s.request))
                    .filter_map(|s| resolve_types(project, &mode, Format::Unknown, &f, &s.request))
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
            if is_relative(&s.request) {
                let next = resolve_types(project, mode, Format::Unknown, file, &s.request)
                    .and_then(|l| sources.get_key_value(&l).map(|(p, _)| p.as_path()));
                if let Some(next) = next
                    && seen.insert(next)
                {
                    stack.push(next);
                }
            } else if s.kind != SpecKind::Augmentation && !oxiclean_core::is_builtin(&s.request) {
                out.push(PackageImport { file, request: &s.request, line: s.line });
            }
        }
    }
    out
}

/// The declaration files `program` loads whatever it imports: the
/// `@types` packages in its `types`, or every visible one; its own
/// declaration files; and the JSX runtime `react-jsx` imports into every
/// module.
fn baseline_entries(project: &Project, program: &TsConfigFile, mode: &Mode) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> =
        program.root_files.iter().filter(|f| is_declaration(f)).cloned().collect();
    let option = |k: &str| program.options.get(k).and_then(Value::as_str);
    let runtime = match option("jsx").map(str::to_lowercase).as_deref() {
        Some("react-jsx") => Some("jsx-runtime"),
        Some("react-jsxdev") => Some("jsx-dev-runtime"),
        _ => None,
    };
    if let Some(runtime) = runtime {
        let source = option("jsxImportSource").unwrap_or("react");
        let from = program.dir.join("index.tsx");
        let format = mode.format(project, &from);
        out.extend(resolve_types(project, mode, format, &from, &format!("{source}/{runtime}")));
    }
    out.extend(ambient_entries(project, program, mode));
    out
}

/// The `@types` packages `program` loads: those in its `types`, or every
/// visible one, in any `node_modules/@types` from its directory up.
fn ambient_entries(project: &Project, program: &TsConfigFile, mode: &Mode) -> Vec<PathBuf> {
    let dirs: Vec<&Path> = program.dir.ancestors().collect();
    let names: Vec<String> = if loads_all_types(project, program) {
        visible_types(&dirs, Path::new("/")).into_iter().map(|(n, _)| n).collect()
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
                let dir = |n: &str| modules.join(n);
                package_file(project, mode, Format::Unknown, &dir(&types_package_name(name)), "")
                    .or_else(|| package_file(project, mode, Format::Unknown, &dir(name), ""))
            })
        })
        .map(|f| fs::canonicalize(&f).unwrap_or(f))
        .collect()
}

/// The declaration files packages load, read once and shared by every
/// program.
struct DeclGraph<'p> {
    project: &'p Project,
    /// The distinct modes of the programs; caches key on their index.
    modes: Vec<Mode>,
    files: DashMap<PathBuf, Arc<DeclFile>>,
    resolved: DashMap<(usize, Format, PathBuf, Request), Option<PathBuf>>,
    closures: DashMap<(usize, PathBuf), Arc<HashSet<PathBuf>>>,
}

impl<'p> DeclGraph<'p> {
    fn new(project: &'p Project, programs: &[&TsConfigFile]) -> Self {
        let mut modes: Vec<Mode> = Vec::new();
        for program in programs {
            let mode = Mode::of(project, program);
            if !modes.contains(&mode) {
                modes.push(mode);
            }
        }
        Self {
            project,
            modes,
            files: DashMap::new(),
            resolved: DashMap::new(),
            closures: DashMap::new(),
        }
    }

    fn mode_id(&self, mode: &Mode) -> usize {
        self.modes.iter().position(|m| m == mode).unwrap_or_default()
    }

    fn lines(&self, file: &Path) -> usize {
        self.files.get(file).map_or(0, |f| f.lines)
    }

    /// The declaration files `entries` load, `entries` included.
    fn closure(&self, mode: usize, entries: &[PathBuf]) -> Arc<HashSet<PathBuf>> {
        if let [entry] = entries
            && let Some(done) = self.closures.get(&(mode, entry.clone()))
        {
            return Arc::clone(&done);
        }
        let mut seen: HashSet<PathBuf> = entries.iter().cloned().collect();
        let mut frontier: Vec<PathBuf> = entries.to_vec();
        while !frontier.is_empty() {
            let next: Vec<PathBuf> = frontier
                .par_iter()
                .flat_map(|f| {
                    let file = self.read(f);
                    file.requests
                        .par_iter()
                        .filter_map(|r| self.resolve(mode, f, r))
                        .collect::<Vec<_>>()
                })
                .collect();
            frontier.clear();
            frontier.extend(next.into_iter().filter(|n| seen.insert(n.clone())));
        }
        let seen = Arc::new(seen);
        if let [entry] = entries {
            self.closures.insert((mode, entry.clone()), Arc::clone(&seen));
        }
        seen
    }

    fn resolve(&self, mode: usize, from: &Path, request: &Request) -> Option<PathBuf> {
        let dir = from.parent().unwrap_or(from).to_path_buf();
        let format = self.modes[mode].format(self.project, from);
        let key = (mode, format, dir, request.clone());
        if let Some(hit) = self.resolved.get(&key) {
            return hit.clone();
        }
        let found = match request {
            Request::Module(m) => resolve_types(self.project, &self.modes[mode], format, from, m),
            // `tsc` adds an extension to a path that has none.
            Request::Path(p) => resolve_file_only(&path_clean::clean(key.2.join(p))),
        }
        .filter(|f| is_declaration(f))
        .map(|f| fs::canonicalize(&f).unwrap_or(f));
        self.resolved.insert(key, found.clone());
        found
    }

    /// `file`, parsed once. Two threads could both miss and parse the same
    /// file, but each frontier holds a file once and closures run one at a
    /// time, so they don't; `entry()` would hold the shard lock while
    /// parsing.
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
    let mut in_block = false;
    for line in src.lines().map(str::trim) {
        if in_block {
            in_block = !line.contains("*/");
            continue;
        }
        if let Some(rest) = line.strip_prefix("/*") {
            in_block = !rest.contains("*/");
            continue;
        }
        if line.is_empty() {
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

    /// pnpm links each package into `.pnpm`, with its dependencies beside
    /// it there; `tsc` follows the links, and reads a file once however
    /// many links lead to it.
    #[cfg(unix)]
    #[test]
    fn follows_pnpm_links_to_real_paths() {
        use std::os::unix::fs::symlink;
        let t = tempfile::TempDir::new().unwrap();
        let root = t.path();
        let write = |path: &str, text: &str| {
            let p = root.join(path);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, text).unwrap();
        };
        let lines = |n: usize, p: &str| {
            (0..n).map(|i| format!("export declare const {p}{i}: number;\n")).collect::<String>()
        };
        write("package.json", r#"{ "name": "app" }"#);
        write(
            ".oxicleanrc.json",
            r#"{ "rules": { "type-bloat": ["error", { "threshold": 100 }] } }"#,
        );
        write(
            "tsconfig.json",
            r#"{ "compilerOptions": { "moduleResolution": "bundler", "module": "esnext", "types": [] }, "include": ["src"] }"#,
        );
        write("src/index.ts", "import { a0 } from \"sdk\";\nexport const x = a0;\n");
        let sdk = "node_modules/.pnpm/sdk@1.0.0/node_modules/sdk";
        let core = "node_modules/.pnpm/core@1.0.0/node_modules/core";
        write(&format!("{sdk}/package.json"), r#"{ "name": "sdk", "types": "index.d.ts" }"#);
        write(
            &format!("{sdk}/index.d.ts"),
            &format!(
                "export * from \"core\";\nexport * from \"./more/../extra\";\n{}",
                lines(50, "a")
            ),
        );
        write(&format!("{sdk}/extra.d.ts"), &lines(30, "e"));
        write(&format!("{core}/package.json"), r#"{ "name": "core", "types": "index.d.ts" }"#);
        write(&format!("{core}/index.d.ts"), &lines(200, "c"));
        fs::create_dir_all(root.join(format!("{sdk}/more"))).unwrap();
        symlink(root.join(sdk), root.join("node_modules/sdk")).unwrap();
        symlink(root.join(core), root.join("node_modules/.pnpm/sdk@1.0.0/node_modules/core"))
            .unwrap();
        let cfg = Config { common: oxiclean_core::CommonArgs::with_root(root.to_path_buf()) };
        let result = run_type_bloat_check(&cfg).unwrap();
        let values: Vec<usize> = result.findings.iter().map(|f| f.value).collect();
        // index.d.ts 52 lines, extra.d.ts 30 and core's 200, each once.
        assert_eq!(values, [282]);
    }

    #[test]
    fn reads_references_after_any_block_comment() {
        let src = "/*\n  Licensed under MIT\n*/\n/** one line */\n/// <reference types=\"node\" />\nexport {};";
        assert_eq!(references(src), [Request::Module("node".into())]);
    }
}
