//! `complex-types`: type shapes that make `tsc` build huge numbers of
//! types.
//!
//! These are read from the syntax, with type aliases followed across
//! files, so they are estimates of cost, not measurements; `tsc
//! --generateTrace` shows what they cost. Reported, as `code`, with an
//! estimate of the types each builds as its value:
//!
//! - `template-product`: a template literal type whose holes are unions:
//!   it expands to every combination, and a conditional type over it runs
//!   once per combination.
//! - `union-intersection`: an intersection of unions, which `tsc`
//!   normalizes into a union of every combination.
//! - `recursive-paths`: a `Paths<T>`-style alias (one that refers to
//!   itself in a template literal inside a mapped type) without a depth
//!   limit, used on a type whose key paths can be counted: a `.json`
//!   import or an object type.
//! - `tuple-arithmetic`: a `Range<N>`-style alias that grows a tuple one
//!   element at a time until its `length` is `N`, used with a literal `N`:
//!   about N²/2 types. `--fix` writes `Range<N>` out as the union
//!   `0 | 1 | … | N-1` when the alias has the usual shape and N is at most
//!   256.
//! - `builder-chain`: a chain of method calls on zod, tRPC, Hono, Elysia
//!   or Drizzle, each of which rebuilds the type built so far: about
//!   links²/2 rebuilds.

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

use dashmap::DashMap;
use oxc_allocator::Allocator;
use oxc_ast::ast::{
    CallExpression, ExportNamedDeclaration, Expression, ImportDeclaration,
    ImportDeclarationSpecifier, ModuleExportName, PropertyKey, Statement, TSLiteral, TSSignature,
    TSTupleElement, TSType, TSTypeAliasDeclaration, TSTypeName, TSTypeOperatorOperator,
    TSTypeQueryExprName, VariableDeclarator,
};
use oxc_ast_visit::{Visit, walk};
use oxc_parser::Parser;
use oxc_span::{GetSpan, SourceType, Span};
use oxiclean_core::{
    CheckResult, Finding, FindingKind, Issue, Project, Result, Severity, Workspace,
    fix::{Fix, FixOp},
    resolve,
};
use rayon::prelude::*;
use serde_json::Value;
use tracing::{debug, info, warn};

use crate::{
    config::{COMPLEX_TYPES_DEFAULT_THRESHOLD, COMPLEX_TYPES_RULE, Config},
    declarations::{in_node_modules, is_declaration, is_relative, is_typescript},
    tsconfig::TsConfigs,
};

/// The largest `N` `--fix` writes `Range<N>` out for.
const MAX_FIXED_RANGE: u64 = 256;

/// How deep aliases are followed into each other.
const MAX_DEPTH: usize = 32;

/// Sizes stop growing here, so products can't overflow.
const CAP: u64 = 1 << 40;

/// Packages whose builder methods each return a rebuilt type.
const BUILDERS: &[&str] = &["zod", "@trpc/server", "hono", "elysia", "drizzle-orm"];

type ResolveCache = DashMap<(PathBuf, String), Option<PathBuf>>;

/// Where a type sits in its file.
#[derive(Debug, Clone, Copy, Default)]
struct At {
    line: u32,
    start: u32,
    end: u32,
}

/// The parts of a TypeScript type these checks look at.
#[derive(Debug, Clone)]
enum Ty {
    Str(String),
    Num(f64),
    /// Another literal: `true`, `null`, a bigint.
    Literal,
    /// `boolean`: `true | false`.
    Bool,
    Never,
    /// A keyword such as `string` or `number`: not a finite union.
    Keyword(&'static str),
    Object(Vec<(String, Ty)>),
    Union(Vec<Ty>),
    Intersection(Vec<Ty>),
    /// A template literal type's holes.
    Template(Vec<Ty>),
    Reference {
        name: String,
        args: Vec<Ty>,
        at: At,
    },
    Keyof(Box<Ty>),
    /// `typeof name`.
    Typeof(String),
    /// `typeof import("request")`.
    TypeofImport(String),
    /// A mapped type's value type.
    Mapped(Box<Ty>),
    Conditional {
        check: Box<Ty>,
        extends: Box<Ty>,
        then: Box<Ty>,
        otherwise: Box<Ty>,
    },
    Indexed(Box<Ty>, Box<Ty>),
    Tuple(Vec<Ty>),
    /// `...T` in a tuple.
    Spread(Box<Ty>),
    /// Anything else, with the types in it.
    Other(Vec<Ty>),
}

impl Ty {
    /// Every type in this one, this one included, depth first.
    fn walk<'s>(&'s self, f: &mut dyn FnMut(&'s Ty) -> bool) {
        if !f(self) {
            return;
        }
        match self {
            Ty::Object(props) => props.iter().for_each(|(_, t)| t.walk(f)),
            Ty::Union(ts)
            | Ty::Intersection(ts)
            | Ty::Template(ts)
            | Ty::Tuple(ts)
            | Ty::Other(ts)
            | Ty::Reference { args: ts, .. } => ts.iter().for_each(|t| t.walk(f)),
            Ty::Keyof(t) | Ty::Mapped(t) | Ty::Spread(t) => t.walk(f),
            Ty::Conditional { check, extends, then, otherwise } => {
                for t in [check, extends, then, otherwise] {
                    t.walk(f);
                }
            }
            Ty::Indexed(a, b) => {
                a.walk(f);
                b.walk(f);
            }
            _ => {}
        }
    }

    /// Whether this type refers to `name`.
    fn refers_to(&self, name: &str) -> bool {
        let mut found = false;
        self.walk(&mut |t| {
            if matches!(t, Ty::Reference { name: n, .. } if n == name) {
                found = true;
            }
            !found
        });
        found
    }

    fn is_reference_to(&self, name: &str) -> bool {
        matches!(self, Ty::Reference { name: n, args, .. } if n == name && args.is_empty())
    }
}

/// A type alias.
#[derive(Debug)]
struct Alias {
    params: Vec<(String, Option<Ty>)>,
    body: Ty,
}

/// A type written somewhere: an alias's body, or an annotation.
#[derive(Debug)]
struct Site {
    /// The alias it's the body of.
    alias: Option<String>,
    ty: Ty,
    line: u32,
}

/// A chain of builder calls.
#[derive(Debug)]
struct Chain {
    line: u32,
    links: u64,
    package: String,
    exported: bool,
}

/// What one file declares, imports and writes.
#[derive(Debug, Default)]
struct FileTypes {
    src: String,
    aliases: HashMap<String, Alias>,
    interfaces: HashSet<String>,
    /// Local name to (module, imported name; `default` or `*` for those).
    imports: HashMap<String, (String, String)>,
    /// Exported name to (module, imported name).
    reexports: HashMap<String, (String, String)>,
    star_reexports: Vec<String>,
    sites: Vec<Site>,
    chains: Vec<Chain>,
}

/// Runs the `complex-types` check with `cfg`.
///
/// # Errors
/// Fails if the root can't be found or read, or the config file is invalid.
pub fn run_complex_types_check(cfg: &Config) -> Result<CheckResult> {
    let workspace = cfg.common.workspace()?;
    run_on(&workspace, &TsConfigs::load(&workspace.project))
}

/// Runs the check on `workspace`, whose tsconfig files are `configs`.
pub(crate) fn run_on(workspace: &Workspace, configs: &TsConfigs) -> Result<CheckResult> {
    info!("Starting {COMPLEX_TYPES_RULE} check");
    let policy =
        workspace.policy(COMPLEX_TYPES_RULE, COMPLEX_TYPES_DEFAULT_THRESHOLD, Severity::Error)?;
    let Some(threshold) = policy.min_threshold() else {
        info!("{COMPLEX_TYPES_RULE} is disabled");
        return Ok(workspace.finish(&policy, Vec::new(), 0));
    };
    let project = &workspace.project;
    let roots: BTreeSet<PathBuf> = configs
        .programs()
        .flat_map(|p| p.root_files.iter())
        .filter(|f| is_typed(f) && !in_node_modules(f))
        .cloned()
        .collect();
    let cache = ResolveCache::new();
    let files = read_files(project, &cache, roots);
    let types = Types { project, cache: &cache, files: &files, threshold: threshold as u64 };
    let mut findings = Vec::new();
    let mut paths: Vec<&PathBuf> = files.keys().collect();
    paths.sort();
    for path in paths {
        types.check_file(path, &mut findings);
    }
    info!(findings = findings.len(), "{COMPLEX_TYPES_RULE} check complete");
    Ok(workspace.finish(&policy, findings, files.len()))
}

/// Reads `roots` and the local files they import, in parallel.
fn read_files(
    project: &Project,
    cache: &ResolveCache,
    roots: BTreeSet<PathBuf>,
) -> HashMap<PathBuf, FileTypes> {
    let mut files: HashMap<PathBuf, FileTypes> = HashMap::new();
    let mut seen: HashSet<PathBuf> = roots.iter().cloned().collect();
    let mut frontier: Vec<PathBuf> = roots.into_iter().collect();
    while !frontier.is_empty() {
        let read: Vec<(PathBuf, FileTypes, Vec<PathBuf>)> = frontier
            .into_par_iter()
            .filter_map(|f| {
                let types = read_types(&f)?;
                let local = types
                    .imports
                    .values()
                    .chain(types.reexports.values())
                    .map(|(m, _)| m)
                    .chain(&types.star_reexports)
                    .filter(|m| is_relative(m))
                    .filter_map(|m| resolve(project, &f, m, cache).ok().flatten())
                    .filter(|l| is_typed(l) && !in_node_modules(l))
                    .collect();
                Some((f, types, local))
            })
            .collect();
        frontier = Vec::new();
        for (f, types, local) in read {
            frontier.extend(local.into_iter().filter(|l| seen.insert(l.clone())));
            files.insert(f, types);
        }
    }
    files
}

/// The types in `file`; `None` if it can't be read or parsed.
fn read_types(file: &Path) -> Option<FileTypes> {
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
    let line_starts: Vec<u32> = std::iter::once(0)
        .chain(src.match_indices('\n').filter_map(|(i, _)| u32::try_from(i + 1).ok()))
        .collect();
    let mut reader = Reader {
        line_starts,
        out: FileTypes::default(),
        in_type: 0,
        exporting: false,
        unannotated_export: false,
        chained: HashSet::new(),
    };
    for statement in &parsed.program.body {
        if let Statement::ImportDeclaration(d) = statement {
            reader.read_import(d);
        }
    }
    reader.visit_program(&parsed.program);
    let mut out = reader.out;
    out.src = src;
    Some(out)
}

struct Reader {
    line_starts: Vec<u32>,
    out: FileTypes,
    /// How many types the visit is inside.
    in_type: usize,
    /// Whether the visit is in an `export` declaration.
    exporting: bool,
    /// Whether the visit is in the initializer of an exported variable
    /// without a type annotation.
    unannotated_export: bool,
    /// The starts of the call chains read so far.
    chained: HashSet<u32>,
}

impl Reader {
    fn line(&self, offset: u32) -> u32 {
        let at = self.line_starts.partition_point(|&s| s <= offset);
        u32::try_from(at).unwrap_or(u32::MAX)
    }

    fn at(&self, span: Span) -> At {
        At { line: self.line(span.start), start: span.start, end: span.end }
    }

    fn read_import(&mut self, d: &ImportDeclaration<'_>) {
        let module = d.source.value.to_string();
        for s in d.specifiers.iter().flatten() {
            let (local, imported) = match s {
                ImportDeclarationSpecifier::ImportSpecifier(s) => {
                    (s.local.name.to_string(), export_name(&s.imported))
                }
                ImportDeclarationSpecifier::ImportDefaultSpecifier(s) => {
                    (s.local.name.to_string(), "default".to_string())
                }
                ImportDeclarationSpecifier::ImportNamespaceSpecifier(s) => {
                    (s.local.name.to_string(), "*".to_string())
                }
            };
            self.out.imports.insert(local, (module.clone(), imported));
        }
    }

    fn ty(&self, t: &TSType<'_>) -> Ty {
        let many = |ts: &mut dyn Iterator<Item = &TSType<'_>>| ts.map(|t| self.ty(t)).collect();
        match t {
            TSType::TSParenthesizedType(p) => self.ty(&p.type_annotation),
            TSType::TSLiteralType(l) => match &l.literal {
                TSLiteral::StringLiteral(s) => Ty::Str(s.value.to_string()),
                TSLiteral::NumericLiteral(n) => Ty::Num(n.value),
                _ => Ty::Literal,
            },
            TSType::TSNullKeyword(_) | TSType::TSUndefinedKeyword(_) | TSType::TSVoidKeyword(_) => {
                Ty::Literal
            }
            TSType::TSBooleanKeyword(_) => Ty::Bool,
            TSType::TSNeverKeyword(_) => Ty::Never,
            TSType::TSStringKeyword(_) => Ty::Keyword("string"),
            TSType::TSNumberKeyword(_) => Ty::Keyword("number"),
            TSType::TSBigIntKeyword(_) => Ty::Keyword("bigint"),
            TSType::TSSymbolKeyword(_) => Ty::Keyword("symbol"),
            TSType::TSAnyKeyword(_) | TSType::TSUnknownKeyword(_) | TSType::TSObjectKeyword(_) => {
                Ty::Keyword("any")
            }
            TSType::TSUnionType(u) => Ty::Union(many(&mut u.types.iter())),
            TSType::TSIntersectionType(i) => Ty::Intersection(many(&mut i.types.iter())),
            TSType::TSTemplateLiteralType(t) => Ty::Template(many(&mut t.types.iter())),
            TSType::TSTypeReference(r) => Ty::Reference {
                name: type_name(&r.type_name),
                args: r
                    .type_arguments
                    .as_ref()
                    .map(|a| many(&mut a.params.iter()))
                    .unwrap_or_default(),
                at: self.at(r.span),
            },
            TSType::TSTypeOperatorType(o) if o.operator == TSTypeOperatorOperator::Keyof => {
                Ty::Keyof(Box::new(self.ty(&o.type_annotation)))
            }
            TSType::TSTypeQuery(q) => match &q.expr_name {
                TSTypeQueryExprName::TSImportType(i) => match &i.argument {
                    TSType::TSLiteralType(l) => match &l.literal {
                        TSLiteral::StringLiteral(s) if i.qualifier.is_none() => {
                            Ty::TypeofImport(s.value.to_string())
                        }
                        _ => Ty::Other(Vec::new()),
                    },
                    _ => Ty::Other(Vec::new()),
                },
                TSTypeQueryExprName::IdentifierReference(id) => Ty::Typeof(id.name.to_string()),
                _ => Ty::Other(Vec::new()),
            },
            TSType::TSTypeLiteral(l) => Ty::Object(
                l.members
                    .iter()
                    .filter_map(|m| match m {
                        TSSignature::TSPropertySignature(p) => {
                            let name = property_name(&p.key)?;
                            let ty = p
                                .type_annotation
                                .as_ref()
                                .map_or(Ty::Keyword("any"), |a| self.ty(&a.type_annotation));
                            Some((name, ty))
                        }
                        _ => None,
                    })
                    .collect(),
            ),
            TSType::TSMappedType(m) => Ty::Mapped(Box::new(
                m.type_annotation.as_ref().map_or(Ty::Keyword("any"), |t| self.ty(t)),
            )),
            TSType::TSConditionalType(c) => Ty::Conditional {
                check: Box::new(self.ty(&c.check_type)),
                extends: Box::new(self.ty(&c.extends_type)),
                then: Box::new(self.ty(&c.true_type)),
                otherwise: Box::new(self.ty(&c.false_type)),
            },
            TSType::TSIndexedAccessType(i) => {
                Ty::Indexed(Box::new(self.ty(&i.object_type)), Box::new(self.ty(&i.index_type)))
            }
            TSType::TSTupleType(t) => Ty::Tuple(
                t.element_types
                    .iter()
                    .map(|e| match e {
                        TSTupleElement::TSRestType(r) => {
                            Ty::Spread(Box::new(self.ty(&r.type_annotation)))
                        }
                        TSTupleElement::TSOptionalType(o) => self.ty(&o.type_annotation),
                        e => e.as_ts_type().map_or(Ty::Other(Vec::new()), |t| self.ty(t)),
                    })
                    .collect(),
            ),
            TSType::TSArrayType(a) => Ty::Other(vec![self.ty(&a.element_type)]),
            TSType::TSNamedTupleMember(m) => match &m.element_type {
                TSTupleElement::TSRestType(r) => Ty::Spread(Box::new(self.ty(&r.type_annotation))),
                e => e.as_ts_type().map_or(Ty::Other(Vec::new()), |t| self.ty(t)),
            },
            _ => Ty::Other(Vec::new()),
        }
    }

    /// The length of the chain of calls `call` ends, and its root.
    fn chain(&mut self, call: &CallExpression<'_>) {
        if !self.chained.insert(call.span.start) {
            return;
        }
        let mut links = 0;
        let mut expr: &Expression<'_> = &call.callee;
        while let Some(member) = expr.as_member_expression() {
            let object = member.object();
            match object.without_parentheses() {
                Expression::CallExpression(inner) => {
                    links += 1;
                    expr = &inner.callee;
                }
                _ => {
                    expr = object;
                    break;
                }
            }
        }
        let root = match expr.without_parentheses() {
            Expression::Identifier(id) => Some(id.name.as_str()),
            Expression::NewExpression(n) => match &n.callee {
                Expression::Identifier(id) => Some(id.name.as_str()),
                _ => None,
            },
            _ => None,
        };
        let package = root
            .and_then(|r| self.out.imports.get(r))
            .map(|(m, _)| m.as_str())
            .filter(|m| BUILDERS.iter().any(|b| m == b || m.starts_with(&format!("{b}/"))));
        if let Some(package) = package
            && links > 0
        {
            let line = self.line(call.span.start);
            self.out.chains.push(Chain {
                line,
                links: links + 1,
                package: package.to_string(),
                exported: self.unannotated_export,
            });
        }
    }
}

impl<'a> Visit<'a> for Reader {
    fn visit_ts_type_alias_declaration(&mut self, it: &TSTypeAliasDeclaration<'a>) {
        let name = it.id.name.to_string();
        let params = it
            .type_parameters
            .iter()
            .flat_map(|p| &p.params)
            .map(|p| (p.name.name.to_string(), p.default.as_ref().map(|d| self.ty(d))))
            .collect();
        let body = self.ty(&it.type_annotation);
        let line = self.line(it.span.start);
        self.out.sites.push(Site { alias: Some(name.clone()), ty: body.clone(), line });
        self.out.aliases.insert(name, Alias { params, body });
    }

    fn visit_ts_interface_declaration(&mut self, it: &oxc_ast::ast::TSInterfaceDeclaration<'a>) {
        self.out.interfaces.insert(it.id.name.to_string());
        walk::walk_ts_interface_declaration(self, it);
    }

    fn visit_ts_type(&mut self, it: &TSType<'a>) {
        if self.in_type == 0 {
            let ty = self.ty(it);
            let line = self.line(it.span().start);
            self.out.sites.push(Site { alias: None, ty, line });
        }
        self.in_type += 1;
        walk::walk_ts_type(self, it);
        self.in_type -= 1;
    }

    fn visit_export_named_declaration(&mut self, it: &ExportNamedDeclaration<'a>) {
        if let Some(source) = &it.source {
            for s in &it.specifiers {
                self.out.reexports.insert(
                    export_name(&s.exported),
                    (source.value.to_string(), export_name(&s.local)),
                );
            }
        }
        let was = self.exporting;
        self.exporting = true;
        walk::walk_export_named_declaration(self, it);
        self.exporting = was;
    }

    fn visit_export_all_declaration(&mut self, it: &oxc_ast::ast::ExportAllDeclaration<'a>) {
        if it.exported.is_none() {
            self.out.star_reexports.push(it.source.value.to_string());
        }
    }

    fn visit_variable_declarator(&mut self, it: &VariableDeclarator<'a>) {
        let was = self.unannotated_export;
        self.unannotated_export = self.exporting && it.id.type_annotation.is_none();
        walk::walk_variable_declarator(self, it);
        self.unannotated_export = was;
    }

    fn visit_call_expression(&mut self, it: &CallExpression<'a>) {
        self.chain(it);
        walk::walk_call_expression(self, it);
    }
}

fn type_name(name: &TSTypeName<'_>) -> String {
    match name {
        TSTypeName::IdentifierReference(id) => id.name.to_string(),
        TSTypeName::QualifiedName(q) => format!("{}.{}", type_name(&q.left), q.right.name),
        TSTypeName::ThisExpression(_) => "this".to_string(),
    }
}

fn export_name(name: &ModuleExportName<'_>) -> String {
    name.name().to_string()
}

fn property_name(key: &PropertyKey<'_>) -> Option<String> {
    key.static_name().map(|n| n.to_string())
}

/// Evaluates sizes and patterns across the files read.
struct Types<'a> {
    project: &'a Project,
    cache: &'a ResolveCache,
    files: &'a HashMap<PathBuf, FileTypes>,
    threshold: u64,
}

/// An alias found by name, with the file it's in.
struct Found<'a> {
    file: &'a Path,
    name: &'a str,
    alias: &'a Alias,
}

impl<'a> Types<'a> {
    /// The alias `name` means in `file`, through imports and re-exports.
    fn alias(&self, file: &'a Path, name: &str, depth: usize) -> Option<Found<'a>> {
        if depth > MAX_DEPTH {
            return None;
        }
        let types = self.files.get(file)?;
        if let Some((key, alias)) = types.aliases.get_key_value(name) {
            return Some(Found { file, name: key, alias });
        }
        let (module, imported) = types.imports.get(name).or_else(|| types.reexports.get(name))?;
        self.exported(file, module, imported, depth + 1)
    }

    fn exported(&self, from: &Path, module: &str, name: &str, depth: usize) -> Option<Found<'a>> {
        let target = resolve(self.project, from, module, self.cache).ok().flatten()?;
        let (target, types) = self.files.get_key_value(&target)?;
        self.alias(target, name, depth).or_else(|| {
            types.star_reexports.iter().find_map(|m| self.exported(target, m, name, depth + 1))
        })
    }

    /// Whether `name` in `file` is an interface.
    fn is_interface(&self, file: &Path, name: &str) -> bool {
        self.files.get(file).is_some_and(|t| t.interfaces.contains(name))
    }

    /// How many members the union `ty` has, if that's finite and known.
    fn size(&self, file: &'a Path, ty: &Ty, args: &Args<'_>, depth: usize) -> Option<u64> {
        if depth > MAX_DEPTH {
            return None;
        }
        let d = depth + 1;
        match ty {
            Ty::Str(_) | Ty::Num(_) | Ty::Literal | Ty::Object(_) | Ty::Tuple(_) => Some(1),
            Ty::Bool => Some(2),
            Ty::Never => Some(0),
            Ty::Union(ts) => ts
                .iter()
                .try_fold(0u64, |acc, t| Some((acc + self.size(file, t, args, d)?).min(CAP))),
            Ty::Intersection(ts) | Ty::Template(ts) => ts.iter().try_fold(1u64, |acc, t| {
                Some(acc.saturating_mul(self.size(file, t, args, d)?).min(CAP))
            }),
            Ty::Keyof(inner) => self.object(file, inner, args, d).map(|p| p.len() as u64),
            Ty::Reference { name, args: given, .. } => {
                if given.is_empty()
                    && let Some((bound_file, bound)) = args.get(name)
                {
                    return self.size(bound_file, bound, &Args::default(), d);
                }
                if self.is_interface(file, name) {
                    return Some(1);
                }
                let found = self.alias(file, name, 0)?;
                let bound = bind(file, found.alias, given)?;
                self.size(found.file, &found.alias.body, &bound, d)
            }
            _ => None,
        }
    }

    /// The properties of the object type `ty`.
    fn object(
        &self,
        file: &'a Path,
        ty: &'a Ty,
        args: &Args<'_>,
        depth: usize,
    ) -> Option<Vec<(String, (&'a Path, &'a Ty))>> {
        if depth > MAX_DEPTH {
            return None;
        }
        match ty {
            Ty::Object(props) => Some(props.iter().map(|(k, t)| (k.clone(), (file, t))).collect()),
            Ty::Reference { name, args: given, .. }
                if given.is_empty() && args.get(name).is_none() =>
            {
                let found = self.alias(file, name, 0)?;
                self.object(found.file, &found.alias.body, &Args::default(), depth + 1)
            }
            _ => None,
        }
    }

    /// How many key paths a `Paths<T>`-style alias builds for `ty`.
    fn paths(&self, file: &'a Path, ty: &'a Ty, depth: usize) -> Option<u64> {
        if depth > MAX_DEPTH {
            return None;
        }
        match ty {
            Ty::TypeofImport(m) if m.ends_with(".json") => self.json_paths(file, m),
            Ty::Typeof(name) => {
                let (m, imported) = self.files.get(file)?.imports.get(name)?;
                (m.ends_with(".json") && imported == "default")
                    .then(|| self.json_paths(file, m))
                    .flatten()
            }
            _ => {
                let props = self.object(file, ty, &Args::default(), 0)?;
                let mut n = 0u64;
                for (_, (f, t)) in props {
                    n += 1 + self.paths(f, t, depth + 1).unwrap_or(0);
                }
                Some(n.min(CAP))
            }
        }
    }

    fn json_paths(&self, from: &Path, module: &str) -> Option<u64> {
        let json = resolve(self.project, from, module, self.cache).ok().flatten()?;
        let text = fs::read_to_string(&json).ok()?;
        let value: Value = serde_json::from_str(&text).ok()?;
        Some(object_keys(&value))
    }

    /// The findings for `path`.
    fn check_file(&self, path: &'a PathBuf, findings: &mut Vec<Finding>) {
        let Some(types) = self.files.get(path) else { return };
        for site in &types.sites {
            self.check_site(path, site, findings);
        }
        for chain in &types.chains {
            let rebuilds = chain.links * (chain.links + 1) / 2;
            if rebuilds < self.threshold {
                continue;
            }
            let mut message = format!(
                "a chain of {} {} calls, each rebuilding the type built so far (about {rebuilds} rebuilds); split it into named parts with explicit types, such as `const users: z.ZodType<User>`",
                chain.links, chain.package
            );
            if chain.exported {
                message.push_str(
                    ", and annotate the export, which declaration emit now writes out in full",
                );
            }
            let issue = Issue::new("builder-chain", chain.package.clone(), message, chain.line);
            findings.push(Finding::new(path.clone(), FindingKind::Issue(issue), clamp(rebuilds)));
        }
    }

    fn check_site(&self, path: &'a PathBuf, site: &'a Site, findings: &mut Vec<Finding>) {
        let label =
            site.alias.as_deref().map_or_else(|| "this type".to_string(), |a| format!("`{a}`"));
        let key = site.alias.clone().unwrap_or_else(|| "type".to_string());
        let mut reported = false;
        site.ty.walk(&mut |t| {
            match t {
                Ty::Template(holes) if !reported => {
                    let sizes: Vec<Option<u64>> =
                        holes.iter().map(|h| self.size(path, h, &Args::default(), 0)).collect();
                    if let Some(product) = product(&sizes) {
                        let parts = sizes.iter().flatten().map(u64::to_string).collect::<Vec<_>>().join(" × ");
                        let message = format!(
                            "{label} expands to {product} strings, every combination of its holes ({parts}), and a conditional type over it runs once per string; accept string and check it at runtime, or take the parts as separate parameters"
                        );
                        let line = site.line;
                        let issue = Issue::new("template-product", key.clone(), message, line);
                        findings.push(Finding::new(path.clone(), FindingKind::Issue(issue), clamp(product)));
                        reported = true;
                    }
                }
                Ty::Intersection(members) if !reported => {
                    let sizes: Vec<Option<u64>> =
                        members.iter().map(|m| self.size(path, m, &Args::default(), 0)).collect();
                    if let Some(product) = product(&sizes) {
                        let parts = sizes.iter().flatten().filter(|s| **s > 1).map(u64::to_string).collect::<Vec<_>>().join(" × ");
                        let message = format!(
                            "{label} intersects unions, which tsc expands into {product} object types, one per combination ({parts}); make one of them a discriminated union and the others optional fields, or nest them as separate properties"
                        );
                        let issue = Issue::new("union-intersection", key.clone(), message, site.line);
                        findings.push(Finding::new(path.clone(), FindingKind::Issue(issue), clamp(product)));
                        reported = true;
                    }
                }
                Ty::Reference { name, args, at } => {
                    self.check_use(path, name, args, *at, findings);
                }
                _ => {}
            }
            true
        });
    }

    /// Findings for a use of a `Paths<T>`- or `Range<N>`-style alias.
    fn check_use(
        &self,
        path: &'a PathBuf,
        name: &str,
        args: &'a [Ty],
        at: At,
        findings: &mut Vec<Finding>,
    ) {
        let Some(found) = self.alias(path, name, 0) else { return };
        if args.is_empty() {
            return;
        }
        if is_paths(found.name, found.alias) {
            let Some(paths) = self.paths(path, &args[0], 0) else { return };
            if paths < self.threshold {
                return;
            }
            let message = format!(
                "`{name}` builds a union of {paths} key paths here, recursing without a depth limit, and pays it again for every type it's given; cap the depth with a counter, generate the union at build time, or accept string"
            );
            let issue = Issue::new("recursive-paths", name.to_string(), message, at.line);
            findings.push(Finding::new(path.clone(), FindingKind::Issue(issue), clamp(paths)));
            return;
        }
        let Some(n_index) = tuple_arithmetic(found.name, found.alias) else { return };
        let Some(Ty::Num(n)) = args.get(n_index) else { return };
        if *n < 0.0 || n.fract() != 0.0 {
            return;
        }
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "checked above"
        )]
        let n = *n as u64;
        let types = n.saturating_mul(n + 1) / 2;
        if types < self.threshold {
            return;
        }
        let mut issue = Issue::new(
            "tuple-arithmetic",
            name.to_string(),
            format!(
                "`{name}<{n}>` builds {n} tuples, each one element longer than the last (about {types} types); write the union out, or use number and check it at runtime"
            ),
            at.line,
        );
        if is_canonical_range(found.name, found.alias) && args.len() == 1 && n <= MAX_FIXED_RANGE {
            let src = &self.files[path].src;
            if let Some(expected) = src.get(at.start as usize..at.end as usize) {
                let text = if n == 0 {
                    "never".to_string()
                } else {
                    (0..n).map(|i| i.to_string()).collect::<Vec<_>>().join(" | ")
                };
                let op = FixOp::Replace {
                    start: at.start as usize,
                    end: at.end as usize,
                    expected: expected.to_string(),
                    text,
                };
                let description = format!("write {expected} out as a union");
                issue = issue.with_fix(Fix::new(path.clone(), description, vec![op]));
            }
        }
        findings.push(Finding::new(path.clone(), FindingKind::Issue(issue), clamp(types)));
    }
}

/// Type arguments bound to an alias's parameters, with the file each was
/// written in.
#[derive(Default)]
struct Args<'t> {
    bound: Vec<(String, &'t Path, &'t Ty)>,
}

impl<'t> Args<'t> {
    fn get(&self, name: &str) -> Option<(&'t Path, &'t Ty)> {
        self.bound.iter().find(|(n, _, _)| n == name).map(|(_, f, t)| (*f, *t))
    }
}

/// `alias`'s parameters bound to `given`, written in `file`, else to
/// their defaults; `None` if one has neither.
fn bind<'t>(file: &'t Path, alias: &'t Alias, given: &'t [Ty]) -> Option<Args<'t>> {
    let mut bound = Vec::new();
    for (i, (name, default)) in alias.params.iter().enumerate() {
        let ty = given.get(i).or(default.as_ref())?;
        bound.push((name.clone(), file, ty));
    }
    Some(Args { bound })
}

/// The product of `sizes`, if at least two are unions and all are known.
fn product(sizes: &[Option<u64>]) -> Option<u64> {
    let known: Vec<u64> = sizes.iter().copied().collect::<Option<_>>()?;
    if known.iter().filter(|s| **s > 1).count() < 2 {
        return None;
    }
    Some(known.iter().fold(1u64, |a, s| a.saturating_mul(*s).min(CAP)))
}

/// Whether `alias` is a `Paths<T>`-style alias without a depth limit: it
/// refers to itself in a template literal inside a mapped type, and takes
/// one parameter.
fn is_paths(name: &str, alias: &Alias) -> bool {
    if alias.params.len() != 1 {
        return false;
    }
    let mut found = false;
    alias.body.walk(&mut |t| {
        if let Ty::Mapped(value) = t {
            value.walk(&mut |v| {
                if matches!(v, Ty::Template(holes) if holes.iter().any(|h| h.refers_to(name))) {
                    found = true;
                }
                !found
            });
        }
        !found
    });
    found
}

/// For a `Range<N>`-style alias, which grows a tuple parameter with
/// `[...A, x]` until `A['length'] extends N`, the index of `N`.
fn tuple_arithmetic(name: &str, alias: &Alias) -> Option<usize> {
    let param = |n: &str| alias.params.iter().position(|(p, _)| p == n);
    let mut out = None;
    alias.body.walk(&mut |t| {
        if let Ty::Conditional { check, extends, then, otherwise } = t
            && let Ty::Indexed(object, index) = check.as_ref()
            && matches!(index.as_ref(), Ty::Str(s) if s == "length")
            && let Ty::Reference { name: a, args, .. } = object.as_ref()
            && args.is_empty()
            && param(a).is_some()
            && let Ty::Reference { name: n, args: n_args, .. } = extends.as_ref()
            && n_args.is_empty()
            && let Some(n_index) = param(n)
            && [then, otherwise].iter().any(|b| grows(b, name, a))
        {
            out = Some(n_index);
        }
        out.is_none()
    });
    out
}

/// Whether `ty` refers to `alias` with a tuple spreading `param`.
fn grows(ty: &Ty, alias: &str, param: &str) -> bool {
    let mut found = false;
    ty.walk(&mut |t| {
        if let Ty::Reference { name, args, .. } = t
            && name == alias
            && args.iter().any(|a| {
                matches!(a, Ty::Tuple(items) if items.iter().any(|i| matches!(i, Ty::Spread(s) if s.is_reference_to(param))))
            })
        {
            found = true;
        }
        !found
    });
    found
}

/// Whether `alias` is exactly
/// `Range<N extends number, A extends unknown[] = []> = A['length'] extends N ? A[number] : Range<N, [...A, A['length']]>`,
/// so `Range<N>` is `0 | 1 | … | N-1`.
fn is_canonical_range(name: &str, alias: &Alias) -> bool {
    let [(n, None), (a, Some(Ty::Tuple(empty)))] = alias.params.as_slice() else { return false };
    if !empty.is_empty() {
        return false;
    }
    let length = |t: &Ty| matches!(t, Ty::Indexed(o, i) if o.is_reference_to(a) && matches!(i.as_ref(), Ty::Str(s) if s == "length"));
    let Ty::Conditional { check, extends, then, otherwise } = &alias.body else { return false };
    let then_ok = matches!(then.as_ref(), Ty::Indexed(o, i) if o.is_reference_to(a) && matches!(i.as_ref(), Ty::Keyword("number")));
    let otherwise_ok = match otherwise.as_ref() {
        Ty::Reference { name: r, args, .. } if r == name => match args.as_slice() {
            [first, Ty::Tuple(items)] => {
                first.is_reference_to(n)
                    && matches!(items.as_slice(), [Ty::Spread(s), last] if s.is_reference_to(a) && length(last))
            }
            _ => false,
        },
        _ => false,
    };
    length(check) && extends.is_reference_to(n) && then_ok && otherwise_ok
}

/// How many keys `value` has, through nested objects.
fn object_keys(value: &Value) -> u64 {
    match value {
        Value::Object(o) => o.values().map(|v| 1 + object_keys(v)).sum(),
        _ => 0,
    }
}

fn clamp(n: u64) -> usize {
    usize::try_from(n).unwrap_or(usize::MAX)
}

/// Whether `file` is TypeScript, declaration files included.
fn is_typed(file: &Path) -> bool {
    is_typescript(file) || is_declaration(file)
}
