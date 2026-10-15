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
//!   normalizes into a union of every combination. An intersection of
//!   literal unions is the intersection of the sets instead, and a union
//!   intersected with itself is that union, as `tsc` reduces them.
//! - `recursive-paths`: a `Paths<T>`-style alias (one that refers to
//!   itself inside a mapped type, in a template literal or through one
//!   helper alias such as `Join<K, Paths<T[K]>>`) without a depth limit,
//!   used on a type whose key paths can be counted: a `.json` import, or an
//!   object type or interface.
//! - `tuple-arithmetic`: a `Range<N>`-style alias that grows a tuple one
//!   element at a time until its `length` is `N`, used with a literal `N`:
//!   about N²/2 types. `tsc` reuses the shorter tuples, so each alias is
//!   reported once per file, for its largest `N`. `--fix` writes every
//!   `Range<N>` in the file out as the union `0 | 1 | … | N-1` when the
//!   alias has the usual shape and N is at most 256.
//! - `builder-chain`: a chain of method calls on zod, tRPC, Hono, Elysia
//!   or Drizzle whose methods each rebuild the type built so far, such as
//!   zod's `.extend()`; refinements such as `.min()` return the same type
//!   and aren't counted. A chain continues through local `const`s it
//!   starts from. Measured on zod 3.25, `.extend()` chains of 10, 20, 40
//!   and 80 links add 1406, 2766, 6086 and 15126 types, so a chain of `n`
//!   links is estimated at 115·n + n² types.
//!
//! Type parameters, and types declared inside namespaces, functions and
//! `declare module` blocks, hide aliases of the same name and are opaque:
//! an alias's body is sized with its parameters unknown, and again with
//! the arguments of each use.

use std::{
    cell::{Cell, RefCell},
    collections::{BTreeSet, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    rc::Rc,
};

use dashmap::DashMap;
use oxc_allocator::Allocator;
use oxc_ast::ast::{
    ArrowFunctionExpression, BindingPatternKind, BlockStatement, CallExpression, Class,
    Declaration, ExportDefaultDeclaration, ExportDefaultDeclarationKind, ExportNamedDeclaration,
    Expression, FormalParameters, Function, FunctionBody, ImportDeclaration,
    ImportDeclarationSpecifier, ModuleExportName, PropertyKey, Statement, TSImportTypeQualifier,
    TSInterfaceDeclaration, TSLiteral, TSModuleDeclaration, TSModuleDeclarationBody, TSSignature,
    TSTupleElement, TSType, TSTypeAliasDeclaration, TSTypeAnnotation, TSTypeName,
    TSTypeOperatorOperator, TSTypeParameterDeclaration, TSTypeQueryExprName, UnaryOperator,
    VariableDeclarator,
};
use oxc_ast_visit::{Visit, walk};
use oxc_parser::Parser;
use oxc_span::{GetSpan, SourceType, Span};
use oxc_syntax::scope::ScopeFlags;
use oxiclean_core::{
    CheckResult, Finding, FindingKind, Issue, Project, Related, Result, Severity, Workspace,
    fix::{Fix, FixOp},
    resolve,
};
use rayon::prelude::*;
use serde_json::Value;
use tracing::{debug, info, warn};

use crate::{
    config::{COMPLEX_TYPES_DEFAULT_THRESHOLD, COMPLEX_TYPES_RULE, Config},
    declarations::{crawl, in_node_modules, is_declaration, is_relative, is_typescript},
    lines::LineIndex,
    tsconfig::TsConfigs,
};

/// The largest `N` `--fix` writes `Range<N>` out for.
const MAX_FIXED_RANGE: u64 = 256;

/// The largest `N` a `Range<N>` is sized for; `tsc` gives up on deeper
/// recursion long before.
const MAX_RANGE: f64 = 10_000.0;

/// How deep aliases are followed into each other.
const MAX_DEPTH: usize = 32;

/// How deep a written type is read; deeper parts are left opaque.
const MAX_TYPE_DEPTH: usize = 256;

/// How deep the instantiations of generic aliases are followed.
const MAX_INSTANTIATION_DEPTH: usize = 4;

/// How many steps sizing the types of one site may take.
const SITE_BUDGET: u32 = 200_000;

/// How many literal members a union is tracked by value for; past this,
/// they're only counted.
const MAX_LITERALS: usize = 1 << 16;

/// Sizes stop growing here, so products can't overflow.
const CAP: u64 = 1 << 40;

/// The types each link of a builder chain adds, beyond the n² term.
const TYPES_PER_LINK: u64 = 115;

/// A package, and its methods that each return a rebuilt type.
type Builder = (&'static str, &'static [&'static str]);

/// Packages whose builder methods each return a rebuilt type, with those
/// methods. A method starting with `extend` counts for each.
const BUILDERS: &[Builder] = &[
    (
        "zod",
        &[
            "extend",
            "merge",
            "pick",
            "omit",
            "partial",
            "deepPartial",
            "required",
            "and",
            "or",
            "augment",
            "setKey",
        ],
    ),
    ("@trpc/server", &["router", "merge", "input", "use"]),
    (
        "hono",
        &[
            "route", "get", "post", "put", "delete", "patch", "options", "all", "on", "use",
            "basePath",
        ],
    ),
    (
        "elysia",
        &[
            "get", "post", "put", "delete", "patch", "options", "head", "all", "ws", "route",
            "use", "group", "guard", "model", "derive", "resolve", "decorate", "state",
        ],
    ),
    (
        "drizzle-orm",
        &[
            "$with",
            "with",
            "select",
            "selectDistinct",
            "selectDistinctOn",
            "from",
            "where",
            "leftJoin",
            "rightJoin",
            "innerJoin",
            "fullJoin",
            "crossJoin",
            "groupBy",
            "having",
            "orderBy",
            "limit",
            "offset",
            "union",
            "unionAll",
            "intersect",
            "except",
            "insert",
            "update",
            "delete",
            "values",
            "set",
            "returning",
            "onConflictDoUpdate",
        ],
    ),
];

type ResolveCache = DashMap<(PathBuf, String), Option<PathBuf>>;

/// Where a type sits in its file.
#[derive(Debug, Clone, Copy, Default)]
struct At {
    line: u32,
    start: u32,
    end: u32,
    /// Whether a union can replace it without parentheses: it's directly
    /// in a union, an alias body, a type argument or an annotation.
    bare: bool,
}

/// The parts of a TypeScript type these checks look at.
#[derive(Debug, Clone)]
enum Ty {
    Str(String),
    Num(f64),
    /// `true`, `false`, `null` or `undefined` (`void` too).
    Atom(&'static str),
    /// Another literal, such as a bigint.
    Literal,
    /// `boolean`: `true | false`.
    Bool,
    Never,
    /// A keyword such as `string` or `number`: not a finite union.
    Keyword(&'static str),
    /// A type parameter, or a type declared in a nested scope, which hides
    /// any alias of the same name.
    Param(String),
    /// An object type's properties, and the types in its other members.
    Object(Vec<(String, Ty)>, Vec<Ty>),
    Union(Vec<Ty>),
    Intersection(Vec<Ty>),
    /// A template literal type's holes.
    Template(Vec<Ty>),
    /// A reference to a named type, from `module` for `import("module")`.
    Reference {
        name: String,
        module: Option<String>,
        args: Vec<Ty>,
        at: At,
    },
    Keyof(Box<Ty>),
    /// `typeof name`.
    Typeof(String),
    /// `typeof import("request")`.
    TypeofImport(String),
    /// A mapped type's key constraint and value type.
    Mapped {
        key: Box<Ty>,
        value: Box<Ty>,
    },
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
    /// Every type in this one, this one included, depth first; `f`
    /// returns whether to go into a type.
    fn walk<'s>(&'s self, f: &mut dyn FnMut(&'s Ty) -> bool) {
        if !f(self) {
            return;
        }
        match self {
            Ty::Object(props, rest) => {
                props.iter().for_each(|(_, t)| t.walk(f));
                rest.iter().for_each(|t| t.walk(f));
            }
            Ty::Union(ts)
            | Ty::Intersection(ts)
            | Ty::Template(ts)
            | Ty::Tuple(ts)
            | Ty::Other(ts)
            | Ty::Reference { args: ts, .. } => ts.iter().for_each(|t| t.walk(f)),
            Ty::Keyof(t) | Ty::Spread(t) => t.walk(f),
            Ty::Mapped { key, value } => {
                key.walk(f);
                value.walk(f);
            }
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

    /// Whether some type in this one matches `pred`.
    fn any(&self, pred: &dyn Fn(&Ty) -> bool) -> bool {
        let mut found = false;
        self.walk(&mut |t| {
            found = found || pred(t);
            !found
        });
        found
    }

    /// Whether this type refers to the alias `name`.
    fn refers_to(&self, name: &str) -> bool {
        self.any(&|t| matches!(t, Ty::Reference { name: n, module: None, .. } if n == name))
    }

    /// Whether this type has a type parameter in it.
    fn has_param(&self) -> bool {
        self.any(&|t| matches!(t, Ty::Param(_)))
    }

    fn is_param(&self, name: &str) -> bool {
        matches!(self, Ty::Param(n) if n == name)
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
    /// The alias it's part of.
    alias: Option<String>,
    /// The type; `None` for the body of `alias`, which the file's aliases
    /// hold.
    ty: Option<Ty>,
    line: u32,
}

/// A chain of method calls.
#[derive(Debug)]
struct Chain {
    line: u32,
    /// The variable the chain starts from: an import or a local `const`.
    root: String,
    /// The methods called, last first.
    methods: Vec<String>,
    /// The `const` the chain is the initializer of.
    name: Option<String>,
    exported: bool,
}

/// What one file declares, imports and writes.
#[derive(Debug, Default)]
struct FileTypes {
    src: String,
    /// Top-level aliases.
    aliases: HashMap<String, Alias>,
    /// Top-level interfaces, as object types of their own members.
    interfaces: HashMap<String, Ty>,
    /// Local name to (module, imported name; `default` or `*` for those).
    imports: HashMap<String, (String, String)>,
    /// Exported name to (module, imported name).
    reexports: HashMap<String, (String, String)>,
    /// Exported name to local name, for `export { A as B }`.
    local_exports: HashMap<String, String>,
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
    let types = Types::new(project, &cache, &files, threshold as u64);
    let mut paths: Vec<&PathBuf> = files.keys().collect();
    paths.sort();
    let findings: Vec<Finding> = paths
        .par_iter()
        .map(|path| types.check_file(path, &files[*path]))
        .collect::<Vec<_>>()
        .into_iter()
        .flatten()
        .collect();
    info!(findings = findings.len(), "{COMPLEX_TYPES_RULE} check complete");
    Ok(workspace.finish(&policy, findings, files.len()))
}

/// Reads `roots` and the local files they import, in parallel.
fn read_files(
    project: &Project,
    cache: &ResolveCache,
    roots: BTreeSet<PathBuf>,
) -> HashMap<PathBuf, FileTypes> {
    crawl(roots, |f| {
        let types = read_types(f)?;
        let local = types
            .imports
            .values()
            .chain(types.reexports.values())
            .map(|(m, _)| m)
            .chain(&types.star_reexports)
            .filter(|m| is_relative(m))
            .filter_map(|m| resolve(project, f, m, cache).ok().flatten())
            .filter(|l| is_typed(l) && !in_node_modules(l))
            .collect();
        Some((types, local))
    })
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
    let mut reader = Reader {
        lines: LineIndex::new(&src),
        out: FileTypes::default(),
        exporting: false,
        unannotated_export: false,
        chained: HashSet::new(),
        chain_names: HashSet::new(),
        declaring: None,
        scope: RefCell::new(Vec::new()),
        depth: Cell::new(0),
        bare: Cell::new(true),
        nested: 0,
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
    lines: LineIndex,
    out: FileTypes,
    /// Whether the visit is in an `export` declaration.
    exporting: bool,
    /// Whether the visit is in the initializer of an exported variable
    /// without a type annotation, or of `export default`.
    unannotated_export: bool,
    /// The starts of the call chains read so far.
    chained: HashSet<u32>,
    /// The `const`s whose initializers are call chains.
    chain_names: HashSet<String>,
    /// The `const` whose initializer starts at this offset.
    declaring: Option<(u32, String)>,
    /// The type parameters, and the types declared in nested scopes, in
    /// scope: they hide aliases of the same name.
    scope: RefCell<Vec<String>>,
    /// How deep `ty` is in the type it's reading.
    depth: Cell<usize>,
    /// Whether the type `ty` reads next is where a union needs no
    /// parentheses.
    bare: Cell<bool>,
    /// How many functions, namespaces and blocks the visit is in.
    nested: usize,
}

impl Reader {
    fn line(&self, offset: u32) -> u32 {
        self.lines.line(offset as usize)
    }

    fn at(&self, span: Span, bare: bool) -> At {
        At { line: self.line(span.start), start: span.start, end: span.end, bare }
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

    /// Puts `names` in scope; returns how many to take out after.
    fn enter(&self, names: impl IntoIterator<Item = String>) -> usize {
        let mut scope = self.scope.borrow_mut();
        let before = scope.len();
        scope.extend(names);
        scope.len() - before
    }

    fn leave(&self, n: usize) {
        let mut scope = self.scope.borrow_mut();
        let len = scope.len();
        scope.truncate(len - n);
    }

    fn in_scope(&self, name: &str) -> bool {
        self.scope.borrow().iter().any(|n| n == name)
    }

    /// `t`, read where a union needs no parentheses.
    fn bare_ty(&self, t: &TSType<'_>) -> Ty {
        self.bare.set(true);
        self.ty(t)
    }

    fn tys(&self, ts: &[TSType<'_>], bare: bool) -> Vec<Ty> {
        ts.iter()
            .map(|t| {
                self.bare.set(bare);
                self.ty(t)
            })
            .collect()
    }

    fn annotation(&self, a: Option<&TSTypeAnnotation<'_>>) -> Option<Ty> {
        a.map(|a| self.bare_ty(&a.type_annotation))
    }

    /// The types in a signature: its parameters' and its return type, with
    /// its type parameters in scope.
    fn signature(
        &self,
        type_params: Option<&TSTypeParameterDeclaration<'_>>,
        params: &FormalParameters<'_>,
        ret: Option<&TSTypeAnnotation<'_>>,
    ) -> Ty {
        let n =
            self.enter(type_params.iter().flat_map(|p| &p.params).map(|p| p.name.name.to_string()));
        let mut parts: Vec<Ty> = type_params
            .iter()
            .flat_map(|p| &p.params)
            .flat_map(|p| p.constraint.iter().chain(&p.default))
            .map(|t| self.bare_ty(t))
            .collect();
        let annotations = params
            .items
            .iter()
            .map(|p| p.pattern.type_annotation.as_deref())
            .chain(params.rest.iter().map(|r| r.argument.type_annotation.as_deref()));
        parts.extend(annotations.filter_map(|a| self.annotation(a)));
        parts.extend(self.annotation(ret));
        self.leave(n);
        Ty::Other(parts)
    }

    /// The `Ty` of `t`, leaving parts deeper than [`MAX_TYPE_DEPTH`]
    /// opaque so that walking, cloning and dropping it stay shallow.
    fn ty(&self, t: &TSType<'_>) -> Ty {
        let bare = self.bare.replace(false);
        let depth = self.depth.get();
        if depth >= MAX_TYPE_DEPTH {
            return Ty::Other(Vec::new());
        }
        self.depth.set(depth + 1);
        let ty = self.read_ty(t, bare);
        self.depth.set(depth);
        ty
    }

    fn read_ty(&self, t: &TSType<'_>, bare: bool) -> Ty {
        match t {
            TSType::TSParenthesizedType(p) => self.bare_ty(&p.type_annotation),
            TSType::TSLiteralType(l) => match &l.literal {
                TSLiteral::StringLiteral(s) => Ty::Str(s.value.to_string()),
                TSLiteral::NumericLiteral(n) => Ty::Num(n.value),
                TSLiteral::BooleanLiteral(b) => Ty::Atom(if b.value { "true" } else { "false" }),
                TSLiteral::UnaryExpression(u) if u.operator == UnaryOperator::UnaryNegation => {
                    match &u.argument {
                        Expression::NumericLiteral(n) => Ty::Num(-n.value),
                        _ => Ty::Literal,
                    }
                }
                _ => Ty::Literal,
            },
            TSType::TSNullKeyword(_) => Ty::Atom("null"),
            TSType::TSUndefinedKeyword(_) | TSType::TSVoidKeyword(_) => Ty::Atom("undefined"),
            TSType::TSBooleanKeyword(_) => Ty::Bool,
            TSType::TSNeverKeyword(_) => Ty::Never,
            TSType::TSStringKeyword(_) => Ty::Keyword("string"),
            TSType::TSNumberKeyword(_) => Ty::Keyword("number"),
            TSType::TSBigIntKeyword(_) => Ty::Keyword("bigint"),
            TSType::TSSymbolKeyword(_) => Ty::Keyword("symbol"),
            TSType::TSAnyKeyword(_) | TSType::TSUnknownKeyword(_) | TSType::TSObjectKeyword(_) => {
                Ty::Keyword("any")
            }
            TSType::TSUnionType(u) => Ty::Union(self.tys(&u.types, true)),
            TSType::TSIntersectionType(i) => Ty::Intersection(self.tys(&i.types, false)),
            TSType::TSTemplateLiteralType(t) => Ty::Template(self.tys(&t.types, true)),
            TSType::TSTypeReference(r) => {
                let name = type_name(&r.type_name);
                let head = name.split('.').next().unwrap_or(&name);
                if self.in_scope(head) {
                    return if head == name { Ty::Param(name) } else { Ty::Other(Vec::new()) };
                }
                let args = r
                    .type_arguments
                    .as_ref()
                    .map(|a| self.tys(&a.params, true))
                    .unwrap_or_default();
                Ty::Reference { name, module: None, args, at: self.at(r.span, bare) }
            }
            TSType::TSImportType(i) => {
                let args = i
                    .type_arguments
                    .as_ref()
                    .map(|a| self.tys(&a.params, true))
                    .unwrap_or_default();
                match (&i.argument, &i.qualifier) {
                    (TSType::TSLiteralType(l), Some(q)) => match &l.literal {
                        TSLiteral::StringLiteral(s) => Ty::Reference {
                            name: import_qualifier(q),
                            module: Some(s.value.to_string()),
                            args,
                            at: self.at(i.span, bare),
                        },
                        _ => Ty::Other(args),
                    },
                    _ => Ty::Other(args),
                }
            }
            TSType::TSTypeOperatorType(o) => {
                let inner = self.ty(&o.type_annotation);
                match o.operator {
                    TSTypeOperatorOperator::Keyof => Ty::Keyof(Box::new(inner)),
                    _ => Ty::Other(vec![inner]),
                }
            }
            TSType::TSTypeQuery(q) => {
                let args = q
                    .type_arguments
                    .as_ref()
                    .map(|a| self.tys(&a.params, true))
                    .unwrap_or_default();
                match &q.expr_name {
                    TSTypeQueryExprName::TSImportType(i) => match &i.argument {
                        TSType::TSLiteralType(l) => match &l.literal {
                            TSLiteral::StringLiteral(s) if i.qualifier.is_none() => {
                                Ty::TypeofImport(s.value.to_string())
                            }
                            _ => Ty::Other(args),
                        },
                        _ => Ty::Other(args),
                    },
                    TSTypeQueryExprName::IdentifierReference(id) => Ty::Typeof(id.name.to_string()),
                    _ => Ty::Other(args),
                }
            }
            TSType::TSTypeLiteral(l) => self.members(&l.members),
            TSType::TSMappedType(m) => {
                let key = m
                    .type_parameter
                    .constraint
                    .as_ref()
                    .map_or(Ty::Other(Vec::new()), |c| self.ty(c));
                let n = self.enter([m.type_parameter.name.name.to_string()]);
                let mut value =
                    m.type_annotation.as_ref().map_or(Ty::Keyword("any"), |t| self.bare_ty(t));
                if let Some(name) = &m.name_type {
                    value = Ty::Other(vec![self.ty(name), value]);
                }
                self.leave(n);
                Ty::Mapped { key: Box::new(key), value: Box::new(value) }
            }
            TSType::TSConditionalType(c) => {
                let check = self.ty(&c.check_type);
                let extends = self.ty(&c.extends_type);
                let mut inferred = Vec::new();
                extends.walk(&mut |t| {
                    if let Ty::Param(p) = t {
                        inferred.push(p.clone());
                    }
                    true
                });
                let n = self.enter(inferred);
                let then = self.bare_ty(&c.true_type);
                self.leave(n);
                let otherwise = self.bare_ty(&c.false_type);
                Ty::Conditional {
                    check: Box::new(check),
                    extends: Box::new(extends),
                    then: Box::new(then),
                    otherwise: Box::new(otherwise),
                }
            }
            TSType::TSInferType(i) => Ty::Param(i.type_parameter.name.name.to_string()),
            TSType::TSIndexedAccessType(i) => Ty::Indexed(
                Box::new(self.ty(&i.object_type)),
                Box::new(self.bare_ty(&i.index_type)),
            ),
            TSType::TSTupleType(t) => Ty::Tuple(
                t.element_types
                    .iter()
                    .map(|e| match e {
                        TSTupleElement::TSRestType(r) => {
                            Ty::Spread(Box::new(self.ty(&r.type_annotation)))
                        }
                        TSTupleElement::TSOptionalType(o) => self.ty(&o.type_annotation),
                        e => e.as_ts_type().map_or(Ty::Other(Vec::new()), |t| self.bare_ty(t)),
                    })
                    .collect(),
            ),
            TSType::TSArrayType(a) => Ty::Other(vec![self.ty(&a.element_type)]),
            TSType::TSNamedTupleMember(m) => match &m.element_type {
                TSTupleElement::TSRestType(r) => Ty::Spread(Box::new(self.ty(&r.type_annotation))),
                e => e.as_ts_type().map_or(Ty::Other(Vec::new()), |t| self.bare_ty(t)),
            },
            TSType::TSFunctionType(f) => {
                self.signature(f.type_parameters.as_deref(), &f.params, Some(&*f.return_type))
            }
            TSType::TSConstructorType(f) => {
                self.signature(f.type_parameters.as_deref(), &f.params, Some(&*f.return_type))
            }
            TSType::TSTypePredicate(p) => {
                Ty::Other(self.annotation(p.type_annotation.as_deref()).into_iter().collect())
            }
            _ => Ty::Other(Vec::new()),
        }
    }

    /// An object type with `members`.
    fn members(&self, members: &[TSSignature<'_>]) -> Ty {
        let mut props = Vec::new();
        let mut rest = Vec::new();
        for m in members {
            match m {
                TSSignature::TSPropertySignature(p) => {
                    let ty =
                        self.annotation(p.type_annotation.as_deref()).unwrap_or(Ty::Keyword("any"));
                    match property_name(&p.key) {
                        Some(name) => props.push((name, ty)),
                        None => rest.push(ty),
                    }
                }
                TSSignature::TSMethodSignature(s) => {
                    let ty = self.signature(
                        s.type_parameters.as_deref(),
                        &s.params,
                        s.return_type.as_deref(),
                    );
                    match property_name(&s.key) {
                        Some(name) => props.push((name, ty)),
                        None => rest.push(ty),
                    }
                }
                TSSignature::TSIndexSignature(s) => {
                    rest.extend(self.annotation(Some(&s.type_annotation)));
                }
                TSSignature::TSCallSignatureDeclaration(s) => rest.push(self.signature(
                    s.type_parameters.as_deref(),
                    &s.params,
                    s.return_type.as_deref(),
                )),
                TSSignature::TSConstructSignatureDeclaration(s) => rest.push(self.signature(
                    s.type_parameters.as_deref(),
                    &s.params,
                    s.return_type.as_deref(),
                )),
            }
        }
        Ty::Object(props, rest)
    }

    /// Records the chain of calls `call` ends.
    fn chain(&mut self, call: &CallExpression<'_>) {
        // Every call in a chain starts where the chain does, and the
        // outermost is visited first: the inner ones are skipped here.
        if !self.chained.insert(call.span.start) {
            return;
        }
        let mut methods = Vec::new();
        let mut expr: &Expression<'_> = &call.callee;
        let mut called = true;
        let root = loop {
            match expr.without_parentheses() {
                Expression::CallExpression(inner) => {
                    expr = &inner.callee;
                    called = true;
                }
                Expression::NewExpression(n) => {
                    expr = &n.callee;
                    called = false;
                }
                Expression::Identifier(id) => break Some(id.name.as_str()),
                e => match e.as_member_expression() {
                    Some(member) => {
                        if called && let Some(name) = member.static_property_name() {
                            methods.push(name.to_string());
                        }
                        called = false;
                        expr = member.object();
                    }
                    None => break None,
                },
            }
        };
        let Some(root) = root else { return };
        let name =
            self.declaring.take_if(|(start, _)| *start == call.span.start).map(|(_, name)| name);
        let known = self.out.imports.contains_key(root) || self.chain_names.contains(root);
        if !known || (methods.is_empty() && name.is_none()) {
            return;
        }
        if let Some(name) = &name {
            self.chain_names.insert(name.clone());
        }
        let line = self.line(call.span.start);
        self.out.chains.push(Chain {
            line,
            root: root.to_string(),
            methods,
            name,
            exported: self.unannotated_export,
        });
    }

    /// Visits a nested scope's `statements`, whose types hide aliases of
    /// the same name.
    fn nested_scope(&mut self, statements: &[Statement<'_>], visit: impl FnOnce(&mut Self)) {
        let n = self.enter(statements.iter().filter_map(declared_type));
        self.nested += 1;
        visit(self);
        self.nested -= 1;
        self.leave(n);
    }

    /// Runs `visit` with the type parameters `params` in scope.
    fn with_type_params(
        &mut self,
        params: Option<&TSTypeParameterDeclaration<'_>>,
        visit: impl FnOnce(&mut Self),
    ) {
        let n = self.enter(params.iter().flat_map(|p| &p.params).map(|p| p.name.name.to_string()));
        visit(self);
        self.leave(n);
    }

    /// Visits a function, whose type parameters are in scope and which
    /// isn't exported however it's written.
    fn function(
        &mut self,
        params: Option<&TSTypeParameterDeclaration<'_>>,
        visit: impl FnOnce(&mut Self),
    ) {
        let exporting = std::mem::take(&mut self.exporting);
        let unannotated = std::mem::take(&mut self.unannotated_export);
        self.with_type_params(params, visit);
        self.exporting = exporting;
        self.unannotated_export = unannotated;
    }
}

impl<'a> Visit<'a> for Reader {
    fn visit_ts_type_alias_declaration(&mut self, it: &TSTypeAliasDeclaration<'a>) {
        let name = it.id.name.to_string();
        let line = self.line(it.span.start);
        let names: Vec<String> = it
            .type_parameters
            .iter()
            .flat_map(|p| &p.params)
            .map(|p| p.name.name.to_string())
            .collect();
        let n = self.enter(names);
        let mut params = Vec::new();
        for p in it.type_parameters.iter().flat_map(|p| &p.params) {
            if let Some(c) = &p.constraint {
                let ty = self.bare_ty(c);
                self.out.sites.push(Site { alias: Some(name.clone()), ty: Some(ty), line });
            }
            let default = p.default.as_ref().map(|d| self.bare_ty(d));
            if let Some(d) = &default {
                self.out.sites.push(Site { alias: Some(name.clone()), ty: Some(d.clone()), line });
            }
            params.push((p.name.name.to_string(), default));
        }
        let body = self.bare_ty(&it.type_annotation);
        self.leave(n);
        if self.nested == 0 {
            self.out.sites.push(Site { alias: Some(name.clone()), ty: None, line });
            self.out.aliases.insert(name, Alias { params, body });
        } else {
            self.out.sites.push(Site { alias: Some(name), ty: Some(body), line });
        }
    }

    fn visit_ts_interface_declaration(&mut self, it: &TSInterfaceDeclaration<'a>) {
        let name = it.id.name.to_string();
        let line = self.line(it.span.start);
        self.with_type_params(it.type_parameters.as_deref(), |reader| {
            let site = reader.members(&it.body.body);
            if let Some(p) = &it.type_parameters {
                reader.visit_ts_type_parameter_declaration(p);
            }
            for h in &it.extends {
                if let Some(args) = &h.type_arguments {
                    reader.visit_ts_type_parameter_instantiation(args);
                }
            }
            if reader.nested == 0 {
                // Declarations of one interface merge.
                if let Some(Ty::Object(all, all_rest)) = reader.out.interfaces.get_mut(&name)
                    && let Ty::Object(props, rest) = &site
                {
                    all.extend(props.iter().cloned());
                    all_rest.extend(rest.iter().cloned());
                } else {
                    reader.out.interfaces.insert(name.clone(), site.clone());
                }
            }
            reader.out.sites.push(Site { alias: Some(name), ty: Some(site), line });
        });
    }

    fn visit_ts_type(&mut self, it: &TSType<'a>) {
        // The whole type is read here; nothing in it needs visiting.
        let ty = self.bare_ty(it);
        let line = self.line(it.span().start);
        self.out.sites.push(Site { alias: None, ty: Some(ty), line });
    }

    fn visit_ts_module_declaration(&mut self, it: &TSModuleDeclaration<'a>) {
        let mut body = it.body.as_ref();
        while let Some(TSModuleDeclarationBody::TSModuleDeclaration(inner)) = body {
            body = inner.body.as_ref();
        }
        let statements = match body {
            Some(TSModuleDeclarationBody::TSModuleBlock(b)) => b.body.as_slice(),
            _ => &[],
        };
        self.nested_scope(statements, |reader| walk::walk_ts_module_declaration(reader, it));
    }

    fn visit_function_body(&mut self, it: &FunctionBody<'a>) {
        self.nested_scope(&it.statements, |reader| walk::walk_function_body(reader, it));
    }

    fn visit_block_statement(&mut self, it: &BlockStatement<'a>) {
        self.nested_scope(&it.body, |reader| walk::walk_block_statement(reader, it));
    }

    fn visit_function(&mut self, it: &Function<'a>, flags: ScopeFlags) {
        self.function(it.type_parameters.as_deref(), |reader| {
            walk::walk_function(reader, it, flags)
        });
    }

    fn visit_arrow_function_expression(&mut self, it: &ArrowFunctionExpression<'a>) {
        self.function(it.type_parameters.as_deref(), |reader| {
            walk::walk_arrow_function_expression(reader, it);
        });
    }

    fn visit_class(&mut self, it: &Class<'a>) {
        self.function(it.type_parameters.as_deref(), |reader| walk::walk_class(reader, it));
    }

    fn visit_export_named_declaration(&mut self, it: &ExportNamedDeclaration<'a>) {
        for s in &it.specifiers {
            let exported = export_name(&s.exported);
            match &it.source {
                Some(source) => {
                    self.out
                        .reexports
                        .insert(exported, (source.value.to_string(), export_name(&s.local)));
                }
                None => {
                    self.out.local_exports.insert(exported, export_name(&s.local));
                }
            }
        }
        let was = self.exporting;
        self.exporting = true;
        walk::walk_export_named_declaration(self, it);
        self.exporting = was;
    }

    fn visit_export_default_declaration(&mut self, it: &ExportDefaultDeclaration<'a>) {
        let expression = !matches!(
            it.declaration,
            ExportDefaultDeclarationKind::FunctionDeclaration(_)
                | ExportDefaultDeclarationKind::ClassDeclaration(_)
                | ExportDefaultDeclarationKind::TSInterfaceDeclaration(_)
        );
        let was = self.unannotated_export;
        self.unannotated_export = expression;
        walk::walk_export_default_declaration(self, it);
        self.unannotated_export = was;
    }

    fn visit_export_all_declaration(&mut self, it: &oxc_ast::ast::ExportAllDeclaration<'a>) {
        if it.exported.is_none() {
            self.out.star_reexports.push(it.source.value.to_string());
        }
    }

    fn visit_variable_declarator(&mut self, it: &VariableDeclarator<'a>) {
        let was = self.unannotated_export;
        self.unannotated_export = self.exporting && it.id.type_annotation.is_none();
        if let BindingPatternKind::BindingIdentifier(id) = &it.id.kind
            && let Some(Expression::CallExpression(call)) =
                it.init.as_ref().map(Expression::without_parentheses)
        {
            self.declaring = Some((call.span.start, id.name.to_string()));
        }
        walk::walk_variable_declarator(self, it);
        self.declaring = None;
        self.unannotated_export = was;
    }

    fn visit_call_expression(&mut self, it: &CallExpression<'a>) {
        self.chain(it);
        walk::walk_call_expression(self, it);
    }
}

/// The type `statement` declares, if it declares one.
fn declared_type(statement: &Statement<'_>) -> Option<String> {
    let declaration = match statement {
        Statement::ExportNamedDeclaration(e) => e.declaration.as_ref()?,
        s => s.as_declaration()?,
    };
    match declaration {
        Declaration::TSTypeAliasDeclaration(d) => Some(d.id.name.to_string()),
        Declaration::TSInterfaceDeclaration(d) => Some(d.id.name.to_string()),
        Declaration::ClassDeclaration(d) => d.id.as_ref().map(|id| id.name.to_string()),
        Declaration::TSEnumDeclaration(d) => Some(d.id.name.to_string()),
        Declaration::TSModuleDeclaration(d) => Some(d.id.name().to_string()),
        _ => None,
    }
}

fn type_name(name: &TSTypeName<'_>) -> String {
    match name {
        TSTypeName::IdentifierReference(id) => id.name.to_string(),
        TSTypeName::QualifiedName(q) => format!("{}.{}", type_name(&q.left), q.right.name),
        TSTypeName::ThisExpression(_) => "this".to_string(),
    }
}

fn import_qualifier(q: &TSImportTypeQualifier<'_>) -> String {
    match q {
        TSImportTypeQualifier::Identifier(id) => id.name.to_string(),
        TSImportTypeQualifier::QualifiedName(q) => {
            format!("{}.{}", import_qualifier(&q.left), q.right.name)
        }
    }
}

fn export_name(name: &ModuleExportName<'_>) -> String {
    name.name().to_string()
}

fn property_name(key: &PropertyKey<'_>) -> Option<String> {
    key.static_name().map(|n| n.to_string())
}

/// A named type found by name, with the file it's in.
#[derive(Clone, Copy)]
struct Found<'a> {
    file: &'a Path,
    name: &'a str,
    decl: Decl<'a>,
}

#[derive(Clone, Copy)]
enum Decl<'a> {
    Alias(&'a Alias),
    /// An interface, as an object type.
    Interface(&'a Ty),
}

/// A type parameter's binding: a type written in `file`, whose own type
/// parameters are bound by `env`.
#[derive(Clone)]
struct Binding<'a> {
    file: &'a Path,
    ty: &'a Ty,
    env: Env<'a>,
}

/// Type parameters bound to types.
#[derive(Clone, Default)]
struct Env<'a>(Rc<Vec<(&'a str, Binding<'a>)>>);

impl<'a> Env<'a> {
    fn get(&self, name: &str) -> Option<&Binding<'a>> {
        self.0.iter().find(|(n, _)| *n == name).map(|(_, b)| b)
    }
}

/// `found`'s parameters bound to `given`, written in `file` with the
/// bindings `env`, else to their defaults, written where `found` is; `None`
/// if one has neither.
fn bind<'a>(
    file: &'a Path,
    env: &Env<'a>,
    found: &Found<'a>,
    alias: &'a Alias,
    given: &'a [Ty],
) -> Option<Env<'a>> {
    let mut bound: Vec<(&'a str, Binding<'a>)> = Vec::new();
    for (i, (name, default)) in alias.params.iter().enumerate() {
        let binding = match given.get(i) {
            Some(ty) => Binding { file, ty, env: env.clone() },
            None => Binding {
                file: found.file,
                ty: default.as_ref()?,
                env: Env(Rc::new(bound.clone())),
            },
        };
        bound.push((name.as_str(), binding));
    }
    Some(Env(Rc::new(bound)))
}

/// A literal type, by value.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Lit {
    Str(String),
    /// A number, by its bits.
    Num(u64),
    Atom(&'static str),
}

/// The members of a union: literals by value, and a count of the others.
#[derive(Debug, Clone, Default)]
struct Members {
    lits: BTreeSet<Lit>,
    others: u64,
}

impl Members {
    fn lit(l: Lit) -> Self {
        Self { lits: BTreeSet::from([l]), others: 0 }
    }

    fn others(n: u64) -> Self {
        Self { lits: BTreeSet::new(), others: n.min(CAP) }
    }

    fn count(&self) -> u64 {
        (self.lits.len() as u64).saturating_add(self.others).min(CAP)
    }

    /// Whether every member is a literal.
    fn literal(&self) -> bool {
        self.others == 0
    }

    fn add(&mut self, other: Members) {
        self.lits.extend(other.lits);
        self.others = self.others.saturating_add(other.others).min(CAP);
        if self.lits.len() > MAX_LITERALS {
            self.others = self.others.saturating_add(self.lits.len() as u64).min(CAP);
            self.lits.clear();
        }
    }
}

/// An intersection, reduced the way `tsc` reduces it.
enum Meet {
    /// Literal unions only: the literals in all of them.
    Set(BTreeSet<Lit>),
    /// Unions with other members: every combination, the sizes multiplied.
    Product(Vec<u64>),
}

/// The largest product found in a site.
struct Product {
    code: &'static str,
    value: u64,
    parts: Vec<u64>,
}

/// A use of a `Range<N>`-style alias.
struct RangeUse {
    n: u64,
    at: At,
    fix: Option<FixOp>,
}

/// Evaluates sizes and patterns across the files read.
struct Types<'a> {
    project: &'a Project,
    cache: &'a ResolveCache,
    files: &'a HashMap<PathBuf, FileTypes>,
    threshold: u64,
    /// The members of aliases used without arguments.
    sizes: DashMap<(&'a Path, &'a str), Option<Members>>,
    /// The key paths of aliases and interfaces.
    paths: DashMap<(&'a Path, &'a str), Option<u64>>,
    /// The keys of `.json` files.
    json: DashMap<PathBuf, Option<u64>>,
}

impl<'a> Types<'a> {
    fn new(
        project: &'a Project,
        cache: &'a ResolveCache,
        files: &'a HashMap<PathBuf, FileTypes>,
        threshold: u64,
    ) -> Self {
        Self {
            project,
            cache,
            files,
            threshold,
            sizes: DashMap::new(),
            paths: DashMap::new(),
            json: DashMap::new(),
        }
    }

    fn resolve(&self, from: &Path, module: &str) -> Option<PathBuf> {
        match resolve(self.project, from, module, self.cache) {
            Ok(Some(target)) => Some(target),
            Ok(None) => {
                debug!("Can't resolve '{module}' from {}", from.display());
                None
            }
            Err(e) => {
                debug!("Can't resolve '{module}' from {}: {e}", from.display());
                None
            }
        }
    }

    /// The type `name` means in `file`, from `module` for
    /// `import("module").name`, through imports and re-exports.
    fn lookup(&self, file: &'a Path, name: &str, module: Option<&str>) -> Option<Found<'a>> {
        let mut visited = HashSet::new();
        match module {
            Some(m) => self.exported(file, m, name, 0, &mut visited),
            None => self.local(file, name, 0, &mut visited),
        }
    }

    fn local(
        &self,
        file: &'a Path,
        name: &str,
        depth: usize,
        visited: &mut HashSet<(&'a Path, String)>,
    ) -> Option<Found<'a>> {
        if depth > MAX_DEPTH {
            return None;
        }
        let types = self.files.get(file)?;
        if let Some(found) = declared(file, types, name) {
            return Some(found);
        }
        if let Some((namespace, rest)) = name.split_once('.') {
            let (module, imported) = types.imports.get(namespace)?;
            return (imported == "*")
                .then(|| self.exported(file, module, rest, depth + 1, visited))
                .flatten();
        }
        let (module, imported) = types.imports.get(name)?;
        self.exported(file, module, imported, depth + 1, visited)
    }

    /// The type `module`, imported from `from`, exports as `name`.
    fn exported(
        &self,
        from: &Path,
        module: &str,
        name: &str,
        depth: usize,
        visited: &mut HashSet<(&'a Path, String)>,
    ) -> Option<Found<'a>> {
        if depth > MAX_DEPTH {
            return None;
        }
        let target = self.resolve(from, module)?;
        let (target, types) = self.files.get_key_value(&target)?;
        if !visited.insert((target.as_path(), name.to_string())) {
            return None;
        }
        if let Some(local) = types.local_exports.get(name) {
            return self.local(target, local, depth + 1, visited);
        }
        if let Some((m, imported)) = types.reexports.get(name) {
            return self.exported(target, m, imported, depth + 1, visited);
        }
        if let Some(found) = declared(target, types, name) {
            return Some(found);
        }
        types.star_reexports.iter().find_map(|m| self.exported(target, m, name, depth + 1, visited))
    }

    /// The findings for `path`, which holds `types`.
    fn check_file(&self, path: &'a Path, types: &'a FileTypes) -> Vec<Finding> {
        let mut findings = Vec::new();
        let mut ranges: Vec<((&'a Path, &'a str), String, Vec<RangeUse>)> = Vec::new();
        for site in &types.sites {
            let ty = match &site.ty {
                Some(ty) => ty,
                None => match site.alias.as_ref().and_then(|a| types.aliases.get(a)) {
                    Some(alias) => &alias.body,
                    None => continue,
                },
            };
            self.check_site(path, types, site, ty, &mut ranges, &mut findings);
        }
        for (_, name, uses) in ranges {
            findings.extend(self.report_ranges(path, &name, uses));
        }
        for chain in &types.chains {
            findings.extend(self.check_chain(path, types, chain));
        }
        findings
    }

    fn check_site(
        &self,
        path: &'a Path,
        types: &'a FileTypes,
        site: &Site,
        ty: &'a Ty,
        ranges: &mut Vec<((&'a Path, &'a str), String, Vec<RangeUse>)>,
        findings: &mut Vec<Finding>,
    ) {
        let ctx = Ctx::new(self);
        let env = Env::default();
        let mut largest: Option<Product> = None;
        ty.walk(&mut |t| {
            match t {
                Ty::Template(_) | Ty::Intersection(_) => {
                    largest = bigger(largest.take(), ctx.product(path, t, &env, 0));
                }
                Ty::Reference { name, module, args, at } => {
                    if args.is_empty() {
                        return true;
                    }
                    let Some(found) = self.lookup(path, name, module.as_deref()) else {
                        return true;
                    };
                    let Decl::Alias(alias) = found.decl else { return true };
                    if is_paths(found.name, alias, &|helper| self.has_template(found.file, helper))
                    {
                        findings.extend(self.check_paths(path, name, args, *at, &ctx));
                    } else if let Some((range, n)) = ctx.range(path, &env, &found, alias, args) {
                        // Only a use of the alias itself can be written out.
                        let fix = ((range.file, range.name) == (found.file, found.name)
                            && is_canonical_range(found.name, alias)
                            && args.len() == 1)
                            .then(|| range_fix(&types.src, n, *at))
                            .flatten();
                        let key = (range.file, range.name);
                        let range_use = RangeUse { n, at: *at, fix };
                        match ranges.iter_mut().find(|(k, _, _)| *k == key) {
                            Some((_, _, uses)) => uses.push(range_use),
                            None => ranges.push((key, range.name.to_string(), vec![range_use])),
                        }
                    } else {
                        let mut seen = Vec::new();
                        largest = bigger(
                            largest.take(),
                            ctx.instantiation(path, &env, &found, alias, args, 0, &mut seen),
                        );
                    }
                }
                _ => {}
            }
            true
        });
        if let Some(product) = largest
            && product.value >= self.threshold
        {
            findings.push(self.report_product(path, site, product));
        }
    }

    /// A finding for the product of a site.
    fn report_product(&self, path: &Path, site: &Site, product: Product) -> Finding {
        let label =
            site.alias.as_deref().map_or_else(|| "this type".to_string(), |a| format!("`{a}`"));
        let key = site.alias.clone().unwrap_or_else(|| "type".to_string());
        let Product { code, value, parts } = product;
        let message = if code == "template-product" {
            format!(
                "{label} expands to {value} strings, every combination of its holes ({}), and a conditional type over it runs once per string; accept string and check it at runtime, or take the parts as separate parameters",
                join_sizes(&parts)
            )
        } else {
            format!(
                "{label} intersects unions, which tsc expands into {value} object types, one per combination ({}); make one of them a discriminated union and the others optional fields, or nest them as separate properties",
                join_sizes(&parts)
            )
        };
        let issue = Issue::new(code, key, message, site.line);
        Finding::new(path.to_path_buf(), FindingKind::Issue(issue), clamp(value))
    }

    /// Whether the alias `name` in `file` has a template literal in its
    /// body.
    fn has_template(&self, file: &'a Path, name: &str) -> bool {
        matches!(
            self.lookup(file, name, None),
            Some(Found { decl: Decl::Alias(a), .. }) if a.body.any(&|t| matches!(t, Ty::Template(_)))
        )
    }

    /// A finding for a use of a `Paths<T>`-style alias.
    fn check_paths(
        &self,
        path: &'a Path,
        name: &str,
        args: &'a [Ty],
        at: At,
        ctx: &Ctx<'_, 'a>,
    ) -> Option<Finding> {
        let [arg] = args else { return None };
        let paths = ctx.paths(path, arg, &Env::default(), 0, &mut Vec::new())?;
        if paths < self.threshold {
            return None;
        }
        let message = format!(
            "`{name}` builds a union of {paths} key paths here, recursing without a depth limit, and pays it again for every type it's given; cap the depth with a counter, generate the union at build time, or accept string"
        );
        let issue = Issue::new("recursive-paths", name.to_string(), message, at.line);
        Some(Finding::new(path.to_path_buf(), FindingKind::Issue(issue), clamp(paths)))
    }

    /// A finding for the uses of one `Range<N>`-style alias in `path`: `tsc`
    /// builds the tuples for the largest `N` and reuses them for the
    /// others.
    fn report_ranges(&self, path: &Path, name: &str, uses: Vec<RangeUse>) -> Option<Finding> {
        // The first of the largest.
        let largest = uses.iter().rev().max_by_key(|u| u.n)?;
        let n = largest.n;
        let types = n.saturating_mul(n.saturating_add(1)) / 2;
        if types < self.threshold {
            return None;
        }
        let others = uses.len() - 1;
        let also = match others {
            0 => String::new(),
            1 => ", which the other use in this file reuses".to_string(),
            k => format!(", which the {k} other uses in this file reuse"),
        };
        let message = format!(
            "`{name}<{n}>` builds {n} tuples, each one element longer than the last (about {types} types){also}; write the union out, or use number and check it at runtime"
        );
        let related = uses
            .iter()
            .filter(|u| !std::ptr::eq(*u, largest))
            .map(|u| {
                Related::new(
                    path.to_path_buf(),
                    u.at.line,
                    format!("`{name}<{}>` reuses them", u.n),
                )
            })
            .collect();
        let mut issue = Issue::new("tuple-arithmetic", name.to_string(), message, largest.at.line)
            .with_related(related);
        if largest.fix.is_some() {
            let ops: Vec<FixOp> = uses.into_iter().filter_map(|u| u.fix).collect();
            let description = if ops.len() == 1 {
                format!("write {name}<{n}> out as a union")
            } else {
                format!("write the {} uses of {name} out as unions", ops.len())
            };
            issue = issue.with_fix(Fix::new(path.to_path_buf(), description, ops));
        }
        Some(Finding::new(path.to_path_buf(), FindingKind::Issue(issue), clamp(types)))
    }

    /// A finding for a chain of builder calls that ends a chain.
    fn check_chain(&self, path: &Path, types: &FileTypes, chain: &Chain) -> Option<Finding> {
        // A chain another continues is counted there.
        if let Some(name) = &chain.name
            && types.chains.iter().any(|c| c.root == *name)
        {
            return None;
        }
        let ((package, _), links) = chain_links(types, chain, 0)?;
        let cost = links.saturating_mul(TYPES_PER_LINK).saturating_add(links.saturating_mul(links));
        if links < 2 || cost < self.threshold {
            return None;
        }
        let example =
            if *package == "zod" { ", such as `const users: z.ZodType<User>`" } else { "" };
        let mut message = format!(
            "a chain of {links} {package} calls, each rebuilding the type built so far (about {cost} types); split it into named parts with explicit types{example}"
        );
        if chain.exported {
            message.push_str(
                ", and annotate the export, which declaration emit now writes out in full",
            );
        }
        let issue = Issue::new("builder-chain", *package, message, chain.line);
        Some(Finding::new(path.to_path_buf(), FindingKind::Issue(issue), clamp(cost)))
    }

    fn json_paths(&self, from: &Path, module: &str) -> Option<u64> {
        let json = self.resolve(from, module)?;
        if let Some(known) = self.json.get(&json) {
            return *known;
        }
        let keys = fs::read_to_string(&json)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .map(|value| object_keys(&value));
        self.json.insert(json, keys);
        keys
    }
}

/// The top-level alias or interface `name` in `file`.
fn declared<'a>(file: &'a Path, types: &'a FileTypes, name: &str) -> Option<Found<'a>> {
    if let Some((name, alias)) = types.aliases.get_key_value(name) {
        return Some(Found { file, name, decl: Decl::Alias(alias) });
    }
    let (name, ty) = types.interfaces.get_key_value(name)?;
    Some(Found { file, name, decl: Decl::Interface(ty) })
}

/// The builder package a chain's calls are on, and how many of them
/// rebuild its type, through the local `const`s it starts from.
fn chain_links(types: &FileTypes, chain: &Chain, depth: usize) -> Option<(&'static Builder, u64)> {
    let (builder, before) = match types.imports.get(&chain.root) {
        Some((module, _)) => (builder(module)?, 0),
        None if depth < MAX_DEPTH => {
            let from = types.chains.iter().find(|c| c.name.as_ref() == Some(&chain.root))?;
            chain_links(types, from, depth + 1)?
        }
        None => return None,
    };
    let (_, methods) = builder;
    let grows = chain.methods.iter().filter(|m| grows_type(methods, m)).count() as u64;
    Some((builder, before + grows))
}

/// Whether `method`, one of a builder's, rebuilds its type.
fn grows_type(methods: &[&str], method: &str) -> bool {
    methods.contains(&method) || method.starts_with("extend")
}

/// The builder package `module` is, or is a subpath of.
fn builder(module: &str) -> Option<&'static Builder> {
    BUILDERS.iter().find(|(b, _)| {
        module.strip_prefix(b).is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    })
}

/// The larger of two products.
fn bigger(a: Option<Product>, b: Option<Product>) -> Option<Product> {
    match (a, b) {
        (Some(a), Some(b)) => Some(if b.value > a.value { b } else { a }),
        (a, b) => a.or(b),
    }
}

/// `sizes` as `a × b × c`, leaving out the 1s.
fn join_sizes(sizes: &[u64]) -> String {
    sizes.iter().filter(|s| **s != 1).map(u64::to_string).collect::<Vec<_>>().join(" × ")
}

/// The `N` of a `Range<N>`, if it's a whole number small enough to size.
fn range_size(arg: &Ty) -> Option<u64> {
    let Ty::Num(n) = arg else { return None };
    if !(0.0..=MAX_RANGE).contains(n) || n.fract() != 0.0 {
        return None;
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a whole number in 0..=MAX_RANGE"
    )]
    Some(*n as u64)
}

/// The whole number `ty` is, through type parameters, if it's small
/// enough to size.
fn number(ty: &Ty, env: &Env<'_>, depth: usize) -> Option<u64> {
    match ty {
        Ty::Param(name) if depth < MAX_DEPTH => {
            let b = env.get(name)?;
            number(b.ty, &b.env, depth + 1)
        }
        _ => range_size(ty),
    }
}

/// The replacement of the `Range<N>` at `at` in `src` by its union.
fn range_fix(src: &str, n: u64, at: At) -> Option<FixOp> {
    if n > MAX_FIXED_RANGE {
        return None;
    }
    let expected = src.get(at.start as usize..at.end as usize)?;
    let union = (0..n).map(|i| i.to_string()).collect::<Vec<_>>().join(" | ");
    let text = match n {
        0 => "never".to_string(),
        1 => union,
        _ if at.bare => union,
        _ => format!("({union})"),
    };
    Some(FixOp::Replace {
        start: at.start as usize,
        end: at.end as usize,
        expected: expected.to_string(),
        text,
    })
}

/// Sizes the types of one site, within a budget.
struct Ctx<'t, 'a> {
    types: &'t Types<'a>,
    budget: Cell<u32>,
    /// Whether a recursive type was cut short, so what's being sized
    /// mustn't be remembered.
    cut: Cell<bool>,
}

impl<'t, 'a> Ctx<'t, 'a> {
    fn new(types: &'t Types<'a>) -> Self {
        Self { types, budget: Cell::new(SITE_BUDGET), cut: Cell::new(false) }
    }

    /// Takes a step from the budget; false once it's spent.
    fn step(&self) -> bool {
        let left = self.budget.get();
        if left == 0 {
            self.cut.set(true);
            return false;
        }
        self.budget.set(left - 1);
        true
    }

    /// The members of the union `ty`, if they're finite and known.
    fn members(&self, file: &'a Path, ty: &'a Ty, env: &Env<'a>, depth: usize) -> Option<Members> {
        if depth > MAX_DEPTH || !self.step() {
            self.cut.set(true);
            return None;
        }
        let d = depth + 1;
        match ty {
            Ty::Str(s) => Some(Members::lit(Lit::Str(s.clone()))),
            Ty::Num(n) => Some(Members::lit(Lit::Num(n.to_bits()))),
            Ty::Atom(a) => Some(Members::lit(Lit::Atom(a))),
            Ty::Bool => Some(Members {
                lits: BTreeSet::from([Lit::Atom("true"), Lit::Atom("false")]),
                others: 0,
            }),
            Ty::Literal | Ty::Object(..) | Ty::Tuple(_) => Some(Members::others(1)),
            Ty::Never => Some(Members::default()),
            Ty::Param(name) => {
                let b = env.get(name)?;
                self.members(b.file, b.ty, &b.env, d)
            }
            Ty::Union(ts) => {
                let mut out = Members::default();
                let mut seen = Vec::new();
                for t in ts {
                    // `A | A` is `A`.
                    if let Some(key) = self.alias_key(file, t, env) {
                        if seen.contains(&key) {
                            continue;
                        }
                        seen.push(key);
                    }
                    out.add(self.members(file, t, env, d)?);
                }
                Some(out)
            }
            Ty::Intersection(ts) => match self.meet(file, ts, env, d)? {
                Meet::Set(lits) => Some(Members { lits, others: 0 }),
                Meet::Product(parts) => Some(Members::others(product(&parts))),
            },
            Ty::Template(holes) => {
                let parts = holes
                    .iter()
                    .map(|h| self.members(file, h, env, d).map(|m| m.count()))
                    .collect::<Option<Vec<_>>>()?;
                Some(Members::others(product(&parts)))
            }
            Ty::Keyof(inner) => {
                let (props, _, _) = self.object(file, inner, env, d)?;
                Some(Members {
                    lits: props.iter().map(|(k, _)| Lit::Str((*k).to_string())).collect(),
                    others: 0,
                })
            }
            Ty::Reference { name, module, args, .. } => {
                let found = self.types.lookup(file, name, module.as_deref())?;
                let alias = match found.decl {
                    Decl::Interface(_) => return Some(Members::others(1)),
                    Decl::Alias(alias) => alias,
                };
                if !args.is_empty() {
                    let bound = bind(file, env, &found, alias, args)?;
                    return self.members(found.file, &alias.body, &bound, d);
                }
                let key = (found.file, found.name);
                if let Some(known) = self.types.sizes.get(&key) {
                    return known.clone();
                }
                let was = self.cut.replace(false);
                let bound = bind(file, env, &found, alias, args);
                let members = bound.and_then(|b| self.members(found.file, &alias.body, &b, d));
                if !self.cut.get() {
                    self.types.sizes.insert(key, members.clone());
                }
                self.cut.set(was || self.cut.get());
                members
            }
            _ => None,
        }
    }

    /// Which alias `ty` refers to without arguments, to tell repeats apart.
    fn alias_key(&self, file: &'a Path, ty: &'a Ty, env: &Env<'a>) -> Option<(&'a Path, &'a str)> {
        match ty {
            Ty::Reference { name, module, args, .. } if args.is_empty() => {
                let found = self.types.lookup(file, name, module.as_deref())?;
                Some((found.file, found.name))
            }
            Ty::Param(name) => {
                let b = env.get(name)?;
                self.alias_key(b.file, b.ty, &b.env)
            }
            _ => None,
        }
    }

    /// The intersection of `members`, reduced.
    fn meet(&self, file: &'a Path, members: &'a [Ty], env: &Env<'a>, depth: usize) -> Option<Meet> {
        let mut seen = Vec::new();
        let mut sets: Option<BTreeSet<Lit>> = None;
        let mut parts = Vec::new();
        for m in members {
            // `A & A` is `A`.
            if let Some(key) = self.alias_key(file, m, env) {
                if seen.contains(&key) {
                    continue;
                }
                seen.push(key);
            }
            let found = self.members(file, m, env, depth)?;
            if found.literal() {
                sets = Some(match sets {
                    Some(s) => s.intersection(&found.lits).cloned().collect(),
                    None => found.lits,
                });
            } else {
                parts.push(found.count());
            }
        }
        if parts.is_empty() {
            return Some(Meet::Set(sets.unwrap_or_default()));
        }
        if let Some(s) = sets {
            parts.insert(0, s.len() as u64);
        }
        Some(Meet::Product(parts))
    }

    /// The product `ty`, a template literal or an intersection, builds,
    /// if it multiplies at least two unions.
    fn product(&self, file: &'a Path, ty: &'a Ty, env: &Env<'a>, depth: usize) -> Option<Product> {
        let (code, parts) = match ty {
            Ty::Template(holes) => (
                "template-product",
                holes
                    .iter()
                    .map(|h| self.members(file, h, env, depth).map(|m| m.count()))
                    .collect::<Option<Vec<_>>>()?,
            ),
            Ty::Intersection(members) => match self.meet(file, members, env, depth)? {
                Meet::Product(parts) => ("union-intersection", parts),
                Meet::Set(_) => return None,
            },
            _ => return None,
        };
        if parts.iter().filter(|s| **s > 1).count() < 2 {
            return None;
        }
        Some(Product { code, value: product(&parts), parts })
    }

    /// The `Range<N>`-style alias a use of `alias` with `given` builds
    /// tuples with, directly or through an alias whose body is a use of
    /// one, and its `N`.
    fn range(
        &self,
        file: &'a Path,
        env: &Env<'a>,
        found: &Found<'a>,
        alias: &'a Alias,
        given: &'a [Ty],
    ) -> Option<(Found<'a>, u64)> {
        if let Some(n_index) = tuple_arithmetic(found.name, alias) {
            return Some((*found, number(given.get(n_index)?, env, 0)?));
        }
        let Ty::Reference { name, module, args, .. } = &alias.body else { return None };
        let inner = self.types.lookup(found.file, name, module.as_deref())?;
        let Decl::Alias(range) = inner.decl else { return None };
        let n_index = tuple_arithmetic(inner.name, range)?;
        let bound = bind(file, env, found, alias, given)?;
        Some((inner, number(args.get(n_index)?, &bound, 0)?))
    }

    /// The largest product the body of `alias`, instantiated with `given`,
    /// builds where it depends on its parameters, through the generic
    /// aliases it uses in turn.
    #[expect(clippy::too_many_arguments, reason = "the instantiation and the path to it")]
    fn instantiation(
        &self,
        file: &'a Path,
        env: &Env<'a>,
        found: &Found<'a>,
        alias: &'a Alias,
        given: &'a [Ty],
        depth: usize,
        seen: &mut Vec<(&'a Path, &'a str)>,
    ) -> Option<Product> {
        let key = (found.file, found.name);
        if depth >= MAX_INSTANTIATION_DEPTH || seen.contains(&key) || !self.step() {
            return None;
        }
        let bound = bind(file, env, found, alias, given)?;
        seen.push(key);
        let mut largest = None;
        alias.body.walk(&mut |t| {
            match t {
                Ty::Template(_) | Ty::Intersection(_) if t.has_param() => {
                    largest = bigger(largest.take(), self.product(found.file, t, &bound, 0));
                }
                Ty::Reference { name, module, args, .. } if args.iter().any(Ty::has_param) => {
                    if let Some(inner) = self.types.lookup(found.file, name, module.as_deref())
                        && let Decl::Alias(a) = inner.decl
                    {
                        let product = self.instantiation(
                            found.file,
                            &bound,
                            &inner,
                            a,
                            args,
                            depth + 1,
                            seen,
                        );
                        largest = bigger(largest.take(), product);
                    }
                }
                _ => {}
            }
            true
        });
        seen.pop();
        largest
    }

    /// The properties of the object type `ty`, with the file and bindings
    /// they're read with.
    #[expect(clippy::type_complexity, reason = "properties, file and bindings")]
    fn object(
        &self,
        file: &'a Path,
        ty: &'a Ty,
        env: &Env<'a>,
        depth: usize,
    ) -> Option<(Vec<(&'a str, &'a Ty)>, &'a Path, Env<'a>)> {
        if depth > MAX_DEPTH || !self.step() {
            return None;
        }
        match ty {
            Ty::Object(props, _) => {
                Some((props.iter().map(|(k, t)| (k.as_str(), t)).collect(), file, env.clone()))
            }
            Ty::Param(name) => {
                let b = env.get(name)?;
                self.object(b.file, b.ty, &b.env, depth + 1)
            }
            Ty::Reference { name, module, args, .. } if args.is_empty() => {
                let found = self.types.lookup(file, name, module.as_deref())?;
                let body = match found.decl {
                    Decl::Alias(alias) => &alias.body,
                    Decl::Interface(ty) => ty,
                };
                self.object(found.file, body, &Env::default(), depth + 1)
            }
            _ => None,
        }
    }

    /// How many key paths a `Paths<T>`-style alias builds for `ty`. A type
    /// met again on the way down ends the path there, as `tsc` does after
    /// reporting it circular.
    fn paths(
        &self,
        file: &'a Path,
        ty: &'a Ty,
        env: &Env<'a>,
        depth: usize,
        on_path: &mut Vec<(&'a Path, &'a str)>,
    ) -> Option<u64> {
        if depth > MAX_DEPTH || !self.step() {
            self.cut.set(true);
            return None;
        }
        match ty {
            Ty::TypeofImport(m) if m.ends_with(".json") => self.types.json_paths(file, m),
            Ty::Typeof(name) => {
                let (m, imported) = self.types.files.get(file)?.imports.get(name)?;
                (m.ends_with(".json") && (imported == "default" || imported == "*"))
                    .then(|| self.types.json_paths(file, m))
                    .flatten()
            }
            Ty::Param(name) => {
                let b = env.get(name)?;
                self.paths(b.file, b.ty, &b.env, depth + 1, on_path)
            }
            Ty::Reference { name, module, args, .. } if args.is_empty() => {
                let found = self.types.lookup(file, name, module.as_deref())?;
                let key = (found.file, found.name);
                if on_path.contains(&key) {
                    self.cut.set(true);
                    return Some(0);
                }
                if let Some(known) = self.types.paths.get(&key) {
                    return *known;
                }
                let was = self.cut.replace(false);
                on_path.push(key);
                let paths = self.object_paths(file, ty, env, depth, on_path);
                on_path.pop();
                if !self.cut.get() {
                    self.types.paths.insert(key, paths);
                }
                self.cut.set(was || self.cut.get());
                paths
            }
            _ => self.object_paths(file, ty, env, depth, on_path),
        }
    }

    fn object_paths(
        &self,
        file: &'a Path,
        ty: &'a Ty,
        env: &Env<'a>,
        depth: usize,
        on_path: &mut Vec<(&'a Path, &'a str)>,
    ) -> Option<u64> {
        let (props, file, env) = self.object(file, ty, env, depth + 1)?;
        let mut n = 0u64;
        for (_, t) in props {
            let below = self.paths(file, t, &env, depth + 1, on_path).unwrap_or(0);
            n = n.saturating_add(1).saturating_add(below).min(CAP);
        }
        Some(n)
    }
}

/// The product of `sizes`, capped.
fn product(sizes: &[u64]) -> u64 {
    sizes.iter().fold(1u64, |a, s| a.saturating_mul(*s).min(CAP))
}

/// Whether `alias` is a `Paths<T>`-style alias without a depth limit: it
/// takes one parameter, and refers to itself inside a mapped type, in a
/// template literal or as an argument of a helper alias that
/// `has_template`, such as `Join<K, Paths<T[K]>>`.
fn is_paths(name: &str, alias: &Alias, has_template: &dyn Fn(&str) -> bool) -> bool {
    if alias.params.len() != 1 {
        return false;
    }
    alias.body.any(&|t| {
        let Ty::Mapped { value, .. } = t else { return false };
        value.any(&|v| match v {
            Ty::Template(holes) => holes.iter().any(|h| h.refers_to(name)),
            Ty::Reference { name: helper, module: None, args, .. } => {
                helper != name && args.iter().any(|a| a.refers_to(name)) && has_template(helper)
            }
            _ => false,
        })
    })
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
            && let Ty::Param(a) = object.as_ref()
            && param(a).is_some()
            && let Ty::Param(n) = extends.as_ref()
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
    let spreads = |a: &Ty| match a {
        Ty::Tuple(items) => items.iter().any(|i| matches!(i, Ty::Spread(s) if s.is_param(param))),
        _ => false,
    };
    ty.any(&|t| matches!(t, Ty::Reference { name, args, .. } if name == alias && args.iter().any(spreads)))
}

/// Whether `alias` is exactly
/// `Range<N extends number, A extends unknown[] = []> = A['length'] extends N ? A[number] : Range<N, [...A, A['length']]>`,
/// so `Range<N>` is `0 | 1 | … | N-1`.
fn is_canonical_range(name: &str, alias: &Alias) -> bool {
    let [(n, None), (a, Some(Ty::Tuple(empty)))] = alias.params.as_slice() else { return false };
    if !empty.is_empty() {
        return false;
    }
    let length = |t: &Ty| {
        matches!(t, Ty::Indexed(o, i)
            if o.is_param(a) && matches!(i.as_ref(), Ty::Str(s) if s == "length"))
    };
    let Ty::Conditional { check, extends, then, otherwise } = &alias.body else { return false };
    let then_ok = matches!(then.as_ref(), Ty::Indexed(o, i)
        if o.is_param(a) && matches!(i.as_ref(), Ty::Keyword("number")));
    let otherwise_ok = match otherwise.as_ref() {
        Ty::Reference { name: r, module: None, args, .. } if r == name => match args.as_slice() {
            [first, Ty::Tuple(items)] => {
                first.is_param(n)
                    && matches!(items.as_slice(), [Ty::Spread(s), last]
                        if s.is_param(a) && length(last))
            }
            _ => false,
        },
        _ => false,
    };
    length(check) && extends.is_param(n) && then_ok && otherwise_ok
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

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    const RANGE: &str = "export type Range<N extends number, A extends number[] = []> = A['length'] extends N\n  ? A[number]\n  : Range<N, [...A, A['length']]>;\n";
    const PATHS: &str = "export type Paths<T> = T extends object ? { [K in keyof T & string]: K | `${K}.${Paths<T[K]>}` }[keyof T & string] : never;\n";

    /// Forty string literals, as a union.
    fn forty(prefix: &str) -> String {
        (1..=40).map(|i| format!("'{prefix}{i}'")).collect::<Vec<_>>().join(" | ")
    }

    /// Writes `files` to a temporary directory, reads them, and runs `f`
    /// on what was read and the path of the first.
    fn with_files<R>(files: &[(&str, &str)], f: impl FnOnce(&Types<'_>, &Path) -> R) -> R {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let mut roots = BTreeSet::new();
        for (name, src) in files {
            let path = root.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, src).unwrap();
            if is_typed(&path) {
                roots.insert(path);
            }
        }
        let project = Project::new(&root);
        let cache = ResolveCache::new();
        let read = read_files(&project, &cache, roots);
        let types = Types::new(&project, &cache, &read, COMPLEX_TYPES_DEFAULT_THRESHOLD as u64);
        f(&types, &root.join(files[0].0))
    }

    /// The findings in `files` at the default threshold.
    fn findings(files: &[(&str, &str)]) -> Vec<Finding> {
        with_files(files, |types, _| {
            let mut paths: Vec<&PathBuf> = types.files.keys().collect();
            paths.sort();
            paths.into_iter().flat_map(|p| types.check_file(p, &types.files[p])).collect()
        })
    }

    /// Each finding as `file:line code key (value)`, with its fix's
    /// description.
    fn check(files: &[(&str, &str)]) -> Vec<String> {
        findings(files)
            .iter()
            .map(|f| {
                let FindingKind::Issue(issue) = &f.kind else { panic!("{f:?}") };
                let file = f.file.file_name().unwrap().to_string_lossy();
                let fix = issue.fix.as_ref().map(|x| format!(" fix: {}", x.description));
                let fix = fix.unwrap_or_default();
                format!("{file}:{} {} {} ({}){fix}", issue.line, issue.code, issue.key, f.value)
            })
            .collect()
    }

    /// The members of the alias `name` in the first of `files`, as
    /// (literals, others).
    fn members(files: &[(&str, &str)], name: &str) -> Option<(usize, u64)> {
        with_files(files, |types, path| {
            let path = types.files.get_key_value(path).unwrap().0.as_path();
            let Some(Found { file, decl: Decl::Alias(alias), .. }) = types.lookup(path, name, None)
            else {
                panic!("no alias {name}")
            };
            let m = Ctx::new(types).members(file, &alias.body, &Env::default(), 0)?;
            Some((m.lits.len(), m.others))
        })
    }

    /// The aliases in `src`.
    fn aliases(src: &str) -> HashMap<String, Alias> {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.ts");
        fs::write(&file, src).unwrap();
        read_types(&file).unwrap().aliases
    }

    /// The fixes of `files`' findings, as the text each writes.
    fn fixes(files: &[(&str, &str)]) -> Vec<String> {
        findings(files)
            .iter()
            .filter_map(|f| match &f.kind {
                FindingKind::Issue(issue) => issue.fix.clone(),
                _ => None,
            })
            .flat_map(|fix| fix.ops)
            .map(|op| match op {
                FixOp::Replace { text, .. } => text.chars().take(9).collect(),
                op => panic!("{op:?}"),
            })
            .collect()
    }

    #[test]
    fn knows_the_canonical_range() {
        let a = aliases(&format!(
            "{RANGE}type Tup<N extends number, A extends unknown[] = []> = A['length'] extends N ? A : Tup<N, [...A, unknown]>;\ntype Off<N extends number, A extends number[] = []> = A['length'] extends N ? A[number] : Off<N, [...A, 1]>;"
        ));
        assert!(is_canonical_range("Range", &a["Range"]));
        assert!(!is_canonical_range("Tup", &a["Tup"]));
        assert!(!is_canonical_range("Off", &a["Off"]));
        // Another alias's name is not this one's.
        assert!(!is_canonical_range("Other", &a["Range"]));
    }

    #[test]
    fn finds_tuple_arithmetic() {
        let a = aliases(&format!(
            "{RANGE}type Rev<A extends unknown[], N extends number> = A['length'] extends N ? A : Rev<[...A, 0], N>;\ntype Flat<N extends number, A extends unknown[] = []> = A['length'] extends N ? A : [];"
        ));
        assert_eq!(tuple_arithmetic("Range", &a["Range"]), Some(0));
        assert_eq!(tuple_arithmetic("Rev", &a["Rev"]), Some(1));
        assert_eq!(tuple_arithmetic("Flat", &a["Flat"]), None);
    }

    #[test]
    fn finds_paths_aliases() {
        let a = aliases(&format!(
            "{PATHS}type Join<K, P> = K extends string ? P extends string ? `${{K}}.${{P}}` : never : never;\ntype J<T> = T extends object ? {{ [K in keyof T]-?: K extends string ? `${{K}}` | Join<K, J<T[K]>> : never }}[keyof T] : '';\ntype D<T, N extends unknown[] = []> = N['length'] extends 4 ? never : {{ [K in keyof T & string]: K | `${{K}}.${{D<T[K], [...N, 0]>}}` }}[keyof T & string];\ntype Flat<T> = {{ [K in keyof T & string]: `${{K}}` }}[keyof T & string];"
        ));
        let none = |_: &str| false;
        assert!(is_paths("Paths", &a["Paths"], &none));
        // Through a helper alias that joins with a template literal.
        assert!(is_paths("J", &a["J"], &|h| h == "Join"));
        assert!(!is_paths("J", &a["J"], &none));
        // A depth limit takes a second parameter.
        assert!(!is_paths("D", &a["D"], &none));
        assert!(!is_paths("Flat", &a["Flat"], &none));
    }

    #[test]
    fn binds_arguments_and_defaults() {
        let a = aliases("type P<A, B = A, C = 'c'> = [A, B, C];\ntype Q<A, B> = [A, B];");
        let given = [Ty::Str("x".to_string())];
        let found = Found { file: Path::new("def.ts"), name: "P", decl: Decl::Alias(&a["P"]) };
        let env = bind(Path::new("use.ts"), &Env::default(), &found, &a["P"], &given).unwrap();
        // Arguments are read where they're written, defaults where the
        // alias is, with the parameters before them bound.
        assert_eq!(env.get("A").unwrap().file, Path::new("use.ts"));
        let b = env.get("B").unwrap();
        assert_eq!(b.file, Path::new("def.ts"));
        assert!(b.ty.is_param("A"));
        assert_eq!(b.env.get("A").unwrap().file, Path::new("use.ts"));
        assert!(matches!(env.get("C").unwrap().ty, Ty::Str(s) if s == "c"));
        let found = Found { file: Path::new("def.ts"), name: "Q", decl: Decl::Alias(&a["Q"]) };
        assert!(bind(Path::new("use.ts"), &Env::default(), &found, &a["Q"], &given).is_none());
    }

    #[test]
    fn reduces_unions_and_intersections() {
        let src = "type L = 'a' | 'b' | 'c';\ntype O = { k: 'a' } | { k: 'b' };\ntype P = { p: 1 } | { p: 2 } | { p: 3 };\n\
            type Dup = L | L | 'a';\ntype DupO = O | O;\ntype Same = L & L & L;\ntype Part = L & ('b' | 'c' | 'd');\n\
            type Disjoint = L & ('x' | 'y');\ntype SameO = O & O;\ntype Both = O & P;\ntype Mixed = L & P;";
        let m = |name: &str| members(&[("a.ts", src)], name);
        assert_eq!(m("Dup"), Some((3, 0)));
        assert_eq!(m("DupO"), Some((0, 2)));
        assert_eq!(m("Same"), Some((3, 0)));
        assert_eq!(m("Part"), Some((2, 0)));
        assert_eq!(m("Disjoint"), Some((0, 0)));
        assert_eq!(m("SameO"), Some((0, 2)));
        assert_eq!(m("Both"), Some((0, 6)));
        assert_eq!(m("Mixed"), Some((0, 9)));
        assert_eq!(product(&[3, 4, 0]), 0);
        assert_eq!(product(&[u64::MAX, 2]), CAP);
    }

    #[test]
    fn counts_object_keys() {
        let value: Value =
            serde_json::from_str(r#"{"a": {"b": 1, "c": {"d": [1]}}, "e": "x"}"#).unwrap();
        assert_eq!(object_keys(&value), 5);
        assert_eq!(object_keys(&Value::Null), 0);
    }

    #[test]
    fn counts_methods_that_grow_types() {
        let zod = builder("zod").unwrap().1;
        assert!(grows_type(zod, "extend") && grows_type(zod, "merge") && grows_type(zod, "pick"));
        assert!(grows_type(zod, "extendSchema"));
        assert!(!grows_type(zod, "min") && !grows_type(zod, "max") && !grows_type(zod, "object"));
        assert!(builder("drizzle-orm/node-postgres").is_some());
        assert!(builder("zod-extra").is_none() && builder("honox").is_none());
    }

    #[test]
    fn skips_ranges_too_large_to_size() {
        let use_ = "import type { Range } from './range';\nexport type A = Range<1e30>;\nexport type B = Range<-5>;\nexport type C = Range<2.5>;\nexport type D = Range<20000>;";
        assert!(check(&[("use.ts", use_), ("range.ts", RANGE)]).is_empty());
    }

    #[test]
    fn caps_deep_types() {
        let deep = format!(
            "type T<X> = {}never;\nexport type U = T<1>;",
            (0..8000).map(|i| format!("X extends {i} ? 'v{i}' : ")).collect::<String>()
        );
        // The parser itself needs the stack; what's read stays shallow.
        let handle = std::thread::Builder::new()
            .stack_size(64 * 1024 * 1024)
            .spawn(move || {
                let a = aliases(&deep);
                fn depth(t: &Ty) -> usize {
                    let mut most = 0;
                    let mut first = true;
                    t.walk(&mut |c| {
                        if std::mem::take(&mut first) {
                            return true;
                        }
                        most = most.max(depth(c));
                        false
                    });
                    most + 1
                }
                depth(&a["T"].body)
            })
            .unwrap();
        assert!(handle.join().unwrap() <= MAX_TYPE_DEPTH + 1);
    }

    #[test]
    fn stops_at_export_star_cycles() {
        let files = [
            (
                "use.ts",
                "import type { User } from './a';\nimport type { Missing } from './b';\nexport type U = Partial<User> | Missing<1>;",
            ),
            ("a.ts", "export * from './b';\nexport interface User { id: string }"),
            ("b.ts", "export * from './a';"),
        ];
        assert!(check(&files).is_empty());
    }

    #[test]
    fn sizes_alias_dags_quickly() {
        let mut src = "type A0 = 'a' | 'b';\ntype B0 = 'c' | 'd';\n".to_string();
        for i in 1..=60 {
            src.push_str(&format!(
                "type A{i} = A{j} | B{j} | `x${{A{j}}}`;\ntype B{i} = B{j} | A{j};\n",
                j = i - 1
            ));
        }
        src.push_str("export type T = `${A60}-${B60}`;\nexport type S = `${A3}-${B2}`;");
        let start = Instant::now();
        let found = check(&[("a.ts", &src)]);
        assert!(start.elapsed() < Duration::from_secs(5));
        assert_eq!(found, [format!("a.ts:123 template-product T ({CAP})")]);
    }

    #[test]
    fn stops_at_recursive_objects() {
        let start = Instant::now();
        let use_ = "import type { Paths } from './paths';\ntype Node = { parent: Node; next: Node; id: string };\ninterface Tree { left: Tree; right: Tree; mid: Tree; v: string }\nexport declare function a(p: Paths<Node>): string;\nexport declare function b(p: Paths<Tree>): string;";
        assert!(check(&[("use.ts", use_), ("paths.ts", PATHS)]).is_empty());
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn parenthesizes_fixes_where_needed() {
        let use_ = "import type { Range } from './range';\nexport type Arr = Range<50>[];\nexport type K = keyof Range<49>;\nexport type I = Range<48>['toFixed'];\nexport type X = Range<47> & number;\nexport type C = Range<46> extends 0 ? 1 : 2;\nexport type P = Range<45>;\nexport type U = 1 | Range<44>;\nexport type G = Promise<Range<43>>;\nexport type One = Range<1>[];";
        assert_eq!(
            fixes(&[("use.ts", use_), ("range.ts", RANGE)]),
            [
                "(0 | 1 | ",
                "(0 | 1 | ",
                "(0 | 1 | ",
                "(0 | 1 | ",
                "(0 | 1 | ",
                "0 | 1 | 2",
                "0 | 1 | 2",
                "0 | 1 | 2",
                "0"
            ]
        );
    }

    #[test]
    fn leaves_scoped_aliases_alone() {
        let src = format!(
            "export namespace Geo {{\n  type Range<N extends number> = {{ lo: 0; hi: N }};\n  export type Lat = Range<180>;\n}}\nexport function f() {{\n  type Range<N> = {{ n: N }};\n  const r: Range<300> = {{ n: 300 }};\n  return r;\n}}\ndeclare module 'x' {{\n  type Range<N> = N;\n  export type Y = Range<400>;\n}}\n{RANGE}export type Top = Range<150>;"
        );
        assert_eq!(
            check(&[("a.ts", &src)]),
            ["a.ts:17 tuple-arithmetic Range (11325) fix: write Range<150> out as a union"]
        );
    }

    #[test]
    fn type_parameters_hide_aliases() {
        let key = format!("export type Key = {};\n", forty("a"));
        let src = format!(
            "{key}export function join<Key extends string>(a: Key): `${{Key}}.${{Key}}.${{Key}}` {{ return a as any; }}\ntype F<Key extends string> = `${{Key}}${{Key}}${{Key}}`;\ntype M = {{ [Key in 'x' | 'y']: `${{Key}}${{Key}}${{Key}}` }};\ntype I<T> = T extends [infer Key] ? `${{Key & string}}${{Key & string}}` : never;\ninterface Box<Key> {{ k: `${{Key & string}}${{Key & string}}` }}\nclass C<Key extends string> {{ m(k: Key): `${{Key}}${{Key}}` {{ return k as any; }} }}"
        );
        assert!(check(&[("a.ts", &src)]).is_empty());
    }

    #[test]
    fn instantiates_nested_generics() {
        let src = format!(
            "type Inner<U> = `${{U}}-${{U}}-${{U}}`;\ntype Outer<T> = Inner<T>;\nexport type O = Outer<{}>;\nexport type Small = Outer<'x' | 'y'>;\ntype Pair<T extends string = 'p' | 'q'> = `${{T}}-${{T}}`;\nexport type Z = Pair<'x'>;",
            forty("a")
        );
        assert_eq!(check(&[("a.ts", &src)]), ["a.ts:3 template-product O (64000)"]);
    }

    #[test]
    fn reports_the_largest_product_of_a_site() {
        let a = format!("type A = {};\n", forty("a"));
        let masked = format!(
            "{a}export type Both = {{ small: `${{'x' | 'y'}}${{'x' | 'y'}}`; big: `${{A}}${{A}}${{A}}` }};"
        );
        assert_eq!(check(&[("a.ts", &masked)]), ["a.ts:2 template-product Both (64000)"]);
        let masked =
            format!("{a}export type Both = ('p' | 'q') & ('p' | 'r') | `${{A}}${{A}}${{A}}`;");
        assert_eq!(check(&[("a.ts", &masked)]), ["a.ts:2 template-product Both (64000)"]);
    }

    #[test]
    fn reports_each_range_once_per_file() {
        let use_ = "import type { Range } from './range';\nexport type P = Range<200>;\nexport type Q = Range<200>;\nexport type W = Range<199>;";
        let found = findings(&[("use.ts", use_), ("range.ts", RANGE)]);
        let [finding] = found.as_slice() else { panic!("{found:?}") };
        let FindingKind::Issue(issue) = &finding.kind else { panic!() };
        assert_eq!((issue.line, finding.value), (2, 20100));
        assert_eq!(issue.related.iter().map(|r| r.line).collect::<Vec<_>>(), [3, 4]);
        assert!(issue.message.contains("which the 2 other uses in this file reuse;"));
        assert_eq!(issue.fix.as_ref().unwrap().ops.len(), 3);
    }

    #[test]
    fn finds_ranges_anywhere_in_a_type() {
        let a = "import type { Range } from './range';\nimport * as R from './range';\nimport type { Rng2 } from './rename';\n\
            export type L1 = () => Range<201>;\nexport type L2 = { [K in Range<202>]: K };\nexport type L3 = readonly Range<203>[];\n\
            export type L5 = { m(): Range<205> };\nexport type L6 = import('./range').Range<206>;\nexport type L7 = R.Range<207>;\n\
            export type L8 = Rng2<208>;\nexport type L10 = { [k: string]: Range<210> };\nexport type L11 = new () => Range<211>;\n\
            export type L13<T = Range<213>> = T;\nexport interface L14 { (x: Range<214>): void }";
        let rename = "import type { Range } from './range';\ntype Local<N extends number> = Range<N>;\nexport type { Local as Rng2 };";
        let found = findings(&[("a.ts", a), ("range.ts", RANGE), ("rename.ts", rename)]);
        let [finding] = found.as_slice() else { panic!("{found:?}") };
        let FindingKind::Issue(issue) = &finding.kind else { panic!() };
        assert_eq!(finding.value, 214 * 215 / 2);
        assert_eq!(issue.related.len(), 10, "{:?}", issue.related);
        // `Rng2<208>` can't be written out in place.
        assert_eq!(issue.fix.as_ref().unwrap().ops.len(), 10);
    }

    #[test]
    fn counts_only_methods_that_grow_types() {
        let extend: String = (1..=40).map(|i| format!(".extend({{ f{i}: z.string() }})")).collect();
        let half: String = (1..=20).map(|i| format!(".extend({{ f{i}: z.string() }})")).collect();
        let min: String = (1..=40).map(|i| format!(".min({i})")).collect();
        let src = format!(
            "import {{ z }} from 'zod';\nimport {{ Hono }} from 'hono';\nimport {{ drizzle }} from 'drizzle-orm/node-postgres';\n\
            export const a = z.object({{}}){extend};\nexport const b = z.string(){min};\n\
            const c = z.object({{}}){half};\nexport const d = c{half};\n\
            export default z.object({{}}){half};\nexport function e() {{ const inner = z.object({{}}){half}; return inner; }}\n\
            export const app = new Hono(){};\nconst db = drizzle('x');\nexport const q = db.select().from(t).leftJoin(a).innerJoin(b).where(c).orderBy(d).groupBy(e).having(f).limit(1).offset(2);",
            (0..12).map(|i| format!(".get('/{i}', h)")).collect::<String>()
        );
        let found = findings(&[("a.ts", &src)]);
        let summary: Vec<(u32, String, usize, bool)> = found
            .iter()
            .map(|f| {
                let FindingKind::Issue(issue) = &f.kind else { panic!() };
                (issue.line, issue.key.clone(), f.value, issue.message.contains("annotate"))
            })
            .collect();
        assert_eq!(
            summary,
            [
                (4, "zod".to_string(), 6200, true),
                (7, "zod".to_string(), 6200, true),
                (8, "zod".to_string(), 2700, true),
                (9, "zod".to_string(), 2700, false),
                (10, "hono".to_string(), 1524, true),
                (12, "drizzle-orm".to_string(), 1250, true),
            ]
        );
    }

    #[test]
    fn counts_paths_through_helpers_imports_and_interfaces() {
        // 10 × (1 + 10 × (1 + 10 × 3)) = 3110 key paths.
        let level = |inner: &str| {
            format!(
                "{{{}}}",
                (0..10).map(|i| format!("\"k{i}\": {inner}")).collect::<Vec<_>>().join(",")
            )
        };
        let json = level(&level(&level(r#"{"a": 1, "b": 2}"#)));
        let paths = "type Join<K, P> = K extends string | number ? P extends string | number ? `${K}${'' extends P ? '' : '.'}${P}` : never : never;\nexport type Paths<T> = T extends object ? { [K in keyof T]-?: K extends string | number ? `${K}` | Join<K, Paths<T[K]>> : never }[keyof T] : '';";
        let a = format!(
            "import * as en from './en.json';\nimport type {{ Paths }} from './paths';\ninterface Big {{ {} }}\ntype Leaf = {{ {} }};\nexport declare function t(key: Paths<typeof en>): string;\nexport declare function u(key: Paths<Big>): string;",
            (0..40).map(|i| format!("i{i}: Leaf;")).collect::<String>(),
            (0..40).map(|i| format!("l{i}: string;")).collect::<String>()
        );
        assert_eq!(
            check(&[("a.ts", &a), ("paths.ts", paths), ("en.json", &json)]),
            ["a.ts:5 recursive-paths Paths (3110)", "a.ts:6 recursive-paths Paths (1640)"]
        );
    }
}
