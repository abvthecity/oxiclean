//! Reading lint configs without running them. A JSON(C) file, or a JS/TS
//! module's export, becomes a [`Val`]: plain data where the source is plain
//! data, a reference to what a package exports (`tseslint.configs.strict`),
//! a call (`defineConfig(...)`), both branches of a condition, or
//! [`Val::Unknown`]. Constants are followed within a module, and imports of
//! the project's own files and workspace packages into the modules they
//! name, a few levels deep.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    Argument, ArrayExpressionElement, Declaration, Expression, ImportDeclarationSpecifier,
    ModuleExportName, ObjectPropertyKind, Program, Statement,
};
use oxc_parser::Parser;
use oxc_span::{GetSpan, SourceType, Span};
use oxiclean_core::Project;
use serde_json::Value;

/// How deep a value is read, so that constants naming each other end.
const MAX_DEPTH: usize = 48;

/// How many imports deep other modules are read.
const MAX_MODULE_DEPTH: usize = 4;

/// Extensions tried when resolving a relative import, in order.
const EXTENSIONS: &[&str] = &["", ".js", ".mjs", ".cjs", ".ts", ".mts", ".cts", ".json"];

/// What a config holds, as far as it can be read without running it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Val {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Item>),
    Obj(Vec<Prop>),
    /// Something a package exports: its `source` and the property path
    /// read from it, `""` for the module itself.
    Import {
        source: String,
        path: String,
    },
    /// A call: the callee as written (`tseslint.config`), the package its
    /// first name comes from, and the arguments.
    Call {
        callee: String,
        source: Option<String>,
        args: Vec<Item>,
    },
    /// One of two values, as `a ? b : c` or `a || b`.
    Either(Box<Val>, Box<Val>),
    /// Anything else.
    Unknown,
}

/// An element of an array, or an argument.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Item {
    pub val: Val,
    /// 1-based line in the config file.
    pub line: u32,
    /// Whether it is spread (`...x`).
    pub spread: bool,
}

/// A property of an object. A spread whose value isn't an object is kept
/// as a property named `...`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Prop {
    pub key: String,
    pub val: Val,
    pub line: u32,
}

/// The key of a spread property that couldn't be read as an object.
pub(crate) const SPREAD: &str = "...";

impl Val {
    /// The value of property `key` of an object: the last one set.
    pub(crate) fn get(&self, key: &str) -> Option<&Val> {
        self.prop(key).map(|p| &p.val)
    }

    /// The property `key` of an object: the last one set.
    pub(crate) fn prop(&self, key: &str) -> Option<&Prop> {
        match self {
            Val::Obj(props) => props.iter().rev().find(|p| p.key == key),
            _ => None,
        }
    }

    /// The property at `path` of nested objects.
    pub(crate) fn at(&self, path: &[&str]) -> Option<&Prop> {
        let (last, parents) = path.split_last()?;
        let mut v = self;
        for k in parents {
            v = v.get(k)?;
        }
        v.prop(last)
    }

    pub(crate) fn as_str(&self) -> Option<&str> {
        match self {
            Val::Str(s) => Some(s),
            _ => None,
        }
    }

    /// The strings of a string or an array of strings, or `None` when any
    /// part isn't a string.
    pub(crate) fn strings(&self) -> Option<Vec<String>> {
        match self {
            Val::Str(s) => Some(vec![s.clone()]),
            Val::Arr(items) => {
                let mut out = Vec::new();
                for i in items {
                    match (&i.val, i.spread) {
                        (Val::Str(s), false) => out.push(s.clone()),
                        (v @ Val::Arr(_), true) => out.extend(v.strings()?),
                        _ => return None,
                    }
                }
                Some(out)
            }
            _ => None,
        }
    }

    /// The object's properties, or none.
    pub(crate) fn props(&self) -> &[Prop] {
        match self {
            Val::Obj(props) => props,
            _ => &[],
        }
    }

    /// Whether the value is known to be truthy (`Some(true)`), falsy
    /// (`Some(false)`), or can't be told.
    pub(crate) fn truthy(&self) -> Option<bool> {
        match self {
            Val::Null => Some(false),
            Val::Bool(b) => Some(*b),
            Val::Num(n) => Some(*n != 0.0),
            Val::Str(s) => Some(!s.is_empty()),
            Val::Arr(_) | Val::Obj(_) => Some(true),
            Val::Either(a, b) => match (a.truthy(), b.truthy()) {
                (Some(x), Some(y)) if x == y => Some(x),
                _ => None,
            },
            Val::Import { .. } | Val::Call { .. } | Val::Unknown => None,
        }
    }
}

/// A config module and what it imports.
#[derive(Debug, Default)]
pub(crate) struct Loaded {
    pub value: Option<Val>,
    /// Every package any module read imports or requires.
    pub packages: Vec<String>,
}

/// Finds the files imports name: relative paths, and workspace packages.
pub(crate) struct Resolver<'p> {
    pub project: &'p Project,
}

impl Resolver<'_> {
    /// The file `request`, imported from a module in `dir`, names, when it
    /// is in the project.
    pub(crate) fn resolve(&self, dir: &Path, request: &str) -> Option<PathBuf> {
        if request.starts_with("./") || request.starts_with("../") || request == "." {
            return file_with_extension(&dir.join(request));
        }
        let (name, sub) = oxiclean_core::split_package_request(request);
        let pkg = self.project.workspace_package(name)?;
        let manifest = self.project.package_json(pkg);
        let key = if sub.is_empty() { ".".to_string() } else { format!("./{sub}") };
        let entry = manifest
            .as_deref()
            .and_then(|m| m.get("exports"))
            .and_then(|e| match e {
                Value::String(_) if key == "." => export_target(e),
                Value::Object(map) if map.keys().any(|k| k.starts_with('.')) => {
                    map.get(&key).and_then(export_target)
                }
                Value::Object(_) if key == "." => export_target(e),
                _ => None,
            })
            .or_else(|| {
                (key == ".")
                    .then(|| manifest.as_deref()?.get("main")?.as_str().map(str::to_string))
                    .flatten()
            });
        match entry {
            Some(e) => file_with_extension(&pkg.join(e)),
            None if sub.is_empty() => file_with_extension(&pkg.join("index")),
            None => file_with_extension(&pkg.join(sub)),
        }
    }
}

/// The file of a package.json `exports` target, preferring the
/// conditions a config loader uses.
fn export_target(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Object(map) => ["import", "default", "require", "node"]
            .iter()
            .find_map(|c| map.get(*c).and_then(export_target))
            .or_else(|| map.values().find_map(export_target)),
        Value::Array(items) => items.iter().find_map(export_target),
        _ => None,
    }
}

/// The file a relative import or `extends` of `path` names.
pub(crate) fn resolve_path(path: &Path) -> Option<PathBuf> {
    file_with_extension(path)
}

/// `path`, or `path` with the first extension that names a file, or its
/// index file.
fn file_with_extension(path: &Path) -> Option<PathBuf> {
    for ext in EXTENSIONS {
        let mut p = path.as_os_str().to_owned();
        p.push(ext);
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    if path.is_dir() {
        return EXTENSIONS[1..]
            .iter()
            .map(|e| path.join(format!("index{e}")))
            .find(|p| p.is_file());
    }
    None
}

/// Where each line of a source file starts.
pub(crate) struct LineIndex {
    starts: Vec<usize>,
}

impl LineIndex {
    pub(crate) fn new(src: &str) -> Self {
        Self {
            starts: std::iter::once(0).chain(src.match_indices('\n').map(|(i, _)| i + 1)).collect(),
        }
    }

    /// The 1-based line `offset` is on.
    pub(crate) fn line(&self, offset: usize) -> u32 {
        u32::try_from(self.starts.partition_point(|&s| s <= offset)).unwrap_or(u32::MAX)
    }
}

/// Whether `file` is read as JSON(C) rather than as a module.
fn is_json(file: &Path) -> bool {
    let name = file.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    name.ends_with(".json") || name.ends_with(".jsonc") || name == ".eslintrc"
}

/// The JSON(C) document in `src`, read as a JS expression so that every
/// value has a line. `None` when it isn't one.
pub(crate) fn json_value(src: &str) -> Option<Val> {
    let src = src.trim_start_matches('\u{feff}');
    // `(` keeps every line where it was.
    let wrapped = format!("({src}\n)");
    let allocator = Allocator::default();
    let ret = Parser::new(&allocator, &wrapped, SourceType::mjs()).parse();
    if !ret.errors.is_empty() || ret.program.body.len() != 1 {
        return None;
    }
    let Statement::ExpressionStatement(s) = &ret.program.body[0] else { return None };
    let mut ctx = Ctx { resolver: None, loaded: Loaded::default(), visiting: Vec::new() };
    let mut reader = Reader {
        lines: LineIndex::new(&wrapped),
        consts: HashMap::new(),
        imports: HashMap::new(),
        dir: PathBuf::new(),
        line_override: None,
        module_depth: 0,
        ctx: &mut ctx,
    };
    let v = reader.value(&s.expression, 0);
    // Only plain data counts as JSON.
    plain(&v).then_some(v)
}

/// Whether `v` is plain data, as JSON is.
fn plain(v: &Val) -> bool {
    match v {
        Val::Null | Val::Bool(_) | Val::Num(_) | Val::Str(_) => true,
        Val::Arr(items) => items.iter().all(|i| !i.spread && plain(&i.val)),
        Val::Obj(props) => props.iter().all(|p| p.key != SPREAD && plain(&p.val)),
        _ => false,
    }
}

/// Reads `file`: its JSON, or what its module exports by default.
pub(crate) fn load(file: &Path, resolver: &Resolver<'_>) -> Loaded {
    if is_json(file) {
        let value = fs::read_to_string(file).ok().and_then(|s| json_value(&s));
        return Loaded { value, packages: Vec::new() };
    }
    let mut ctx = Ctx { resolver: Some(resolver), loaded: Loaded::default(), visiting: Vec::new() };
    let value = module_export(&mut ctx, file, None, None, 0);
    let mut loaded = ctx.loaded;
    loaded.value = value;
    loaded.packages.sort();
    loaded.packages.dedup();
    loaded
}

struct Ctx<'r> {
    resolver: Option<&'r Resolver<'r>>,
    loaded: Loaded,
    /// The modules being read, so imports that cycle end.
    visiting: Vec<PathBuf>,
}

/// What a module exports as `name`, or by default. `line` replaces the
/// lines of everything read from another module with the line that
/// imports it.
fn module_export(
    ctx: &mut Ctx<'_>,
    file: &Path,
    name: Option<&str>,
    line: Option<u32>,
    module_depth: usize,
) -> Option<Val> {
    if module_depth > MAX_MODULE_DEPTH || ctx.visiting.iter().any(|f| f == file) {
        return None;
    }
    if is_json(file) {
        let v = json_value(&fs::read_to_string(file).ok()?)?;
        return Some(match line {
            Some(l) => relined(v, l),
            None => v,
        });
    }
    let src = fs::read_to_string(file).ok()?;
    let source_type = SourceType::from_path(file).ok()?;
    let allocator = Allocator::default();
    let ret = Parser::new(&allocator, &src, source_type).parse();
    if ret.panicked {
        return None;
    }
    ctx.visiting.push(file.to_path_buf());
    let dir = file.parent().map(Path::to_path_buf).unwrap_or_default();
    let out = read_program(ctx, &ret.program, &src, dir, name, line, module_depth);
    ctx.visiting.pop();
    out
}

/// `v` with every line set to `line`.
fn relined(v: Val, line: u32) -> Val {
    match v {
        Val::Arr(items) => Val::Arr(
            items
                .into_iter()
                .map(|i| Item { val: relined(i.val, line), line, spread: i.spread })
                .collect(),
        ),
        Val::Obj(props) => Val::Obj(
            props
                .into_iter()
                .map(|p| Prop { key: p.key, val: relined(p.val, line), line })
                .collect(),
        ),
        other => other,
    }
}

fn read_program<'a>(
    ctx: &mut Ctx<'_>,
    program: &Program<'a>,
    src: &str,
    dir: PathBuf,
    name: Option<&str>,
    line_override: Option<u32>,
    module_depth: usize,
) -> Option<Val> {
    let mut consts: HashMap<&str, &Expression<'a>> = HashMap::new();
    let mut imports: HashMap<&str, (String, Option<String>)> = HashMap::new();
    let mut exported: Option<&Expression<'a>> = None;
    let mut named: HashMap<String, &Expression<'a>> = HashMap::new();
    // `export { a as b }`: b → a.
    let mut aliases: Vec<(String, String)> = Vec::new();
    for stmt in &program.body {
        match stmt {
            Statement::ImportDeclaration(decl) => {
                let source = decl.source.value.to_string();
                ctx.loaded.packages.push(source.clone());
                for spec in decl.specifiers.iter().flatten() {
                    match spec {
                        ImportDeclarationSpecifier::ImportSpecifier(s) => {
                            let imported = match &s.imported {
                                ModuleExportName::IdentifierName(n) => n.name.to_string(),
                                ModuleExportName::IdentifierReference(n) => n.name.to_string(),
                                ModuleExportName::StringLiteral(n) => n.value.to_string(),
                            };
                            let imported = (imported != "default").then_some(imported);
                            imports.insert(s.local.name.as_str(), (source.clone(), imported));
                        }
                        ImportDeclarationSpecifier::ImportDefaultSpecifier(s) => {
                            imports.insert(s.local.name.as_str(), (source.clone(), None));
                        }
                        ImportDeclarationSpecifier::ImportNamespaceSpecifier(s) => {
                            imports.insert(s.local.name.as_str(), (source.clone(), None));
                        }
                    }
                }
            }
            Statement::VariableDeclaration(decl) => declare(decl, &mut consts),
            Statement::ExportDefaultDeclaration(decl) => {
                exported = decl.declaration.as_expression();
            }
            Statement::ExportNamedDeclaration(decl) => {
                if let Some(Declaration::VariableDeclaration(v)) = &decl.declaration {
                    declare(v, &mut consts);
                    for d in &v.declarations {
                        if let (Some(n), Some(init)) = (d.id.get_identifier_name(), &d.init) {
                            named.insert(n.to_string(), init);
                        }
                    }
                }
                for s in &decl.specifiers {
                    aliases.push((s.exported.name().to_string(), s.local.name().to_string()));
                }
            }
            Statement::ExpressionStatement(s) => {
                if let Expression::AssignmentExpression(a) = &s.expression
                    && a.left
                        .as_member_expression()
                        .is_some_and(|m| m.is_specific_member_access("module", "exports"))
                {
                    exported = Some(&a.right);
                }
            }
            _ => {}
        }
    }
    // A constant changed after it is declared, as `config.push({...})`,
    // isn't what its initializer says.
    for stmt in &program.body {
        if let Statement::ExpressionStatement(s) = stmt
            && let Some(name) = mutated(&s.expression)
        {
            consts.remove(name);
        }
    }
    // `require('x')` imports too.
    for e in consts.values() {
        if let Expression::CallExpression(c) = e.get_inner_expression()
            && c.is_require_call()
            && let Some(Argument::StringLiteral(s)) = c.arguments.first()
        {
            ctx.loaded.packages.push(s.value.to_string());
        }
    }
    let mut reader = Reader {
        lines: LineIndex::new(src),
        consts,
        imports,
        dir,
        line_override,
        module_depth,
        ctx,
    };
    match name {
        None => {
            if exported.is_none()
                && let Some((_, local)) = aliases.iter().find(|(e, _)| e == "default")
            {
                exported = reader.consts.get(local.as_str()).copied();
            }
            Some(reader.value(exported?, 0))
        }
        Some(n) => {
            let e = named.get(n).copied().or_else(|| {
                let (_, local) = aliases.iter().find(|(e, _)| e == n)?;
                reader.consts.get(local.as_str()).copied()
            });
            match e {
                Some(e) => Some(reader.value(e, 0)),
                // `module.exports = { n }`.
                None => {
                    let v = reader.value(exported?, 0);
                    v.get(n).cloned()
                }
            }
        }
    }
}

/// The variable expression `e` changes: `x.push(...)` and the like,
/// `x.y = ...`, `x = ...` and `Object.assign(x, ...)`.
fn mutated<'s>(e: &'s Expression<'_>) -> Option<&'s str> {
    let root = |e: &'s Expression<'_>| -> Option<&'s str> {
        let mut cur = e.get_inner_expression();
        loop {
            match cur {
                Expression::Identifier(id) => return Some(id.name.as_str()),
                Expression::StaticMemberExpression(m) => cur = m.object.get_inner_expression(),
                Expression::ComputedMemberExpression(m) => cur = m.object.get_inner_expression(),
                _ => return None,
            }
        }
    };
    match e.get_inner_expression() {
        Expression::CallExpression(c) => match c.callee.get_inner_expression() {
            Expression::StaticMemberExpression(m)
                if m.object.is_specific_id("Object") && m.property.name == "assign" =>
            {
                c.arguments.first().and_then(Argument::as_expression).and_then(root)
            }
            Expression::StaticMemberExpression(m)
                if matches!(
                    m.property.name.as_str(),
                    "push" | "unshift" | "splice" | "pop" | "shift" | "fill" | "reverse" | "sort"
                ) =>
            {
                root(&m.object)
            }
            _ => None,
        },
        Expression::AssignmentExpression(a) => match &a.left {
            oxc_ast::ast::AssignmentTarget::AssignmentTargetIdentifier(id) => {
                Some(id.name.as_str())
            }
            left => left.as_member_expression().and_then(|m| root(m.object())),
        },
        _ => None,
    }
}

/// Records the `const`s, `let`s and `var`s `decl` declares.
fn declare<'s, 'a>(
    decl: &'s oxc_ast::ast::VariableDeclaration<'a>,
    consts: &mut HashMap<&'s str, &'s Expression<'a>>,
) {
    for d in &decl.declarations {
        if let (Some(name), Some(init)) = (d.id.get_identifier_name(), &d.init) {
            consts.insert(name.as_str(), init);
        }
    }
}

struct Reader<'s, 'a, 'c, 'r> {
    lines: LineIndex,
    consts: HashMap<&'s str, &'s Expression<'a>>,
    imports: HashMap<&'s str, (String, Option<String>)>,
    dir: PathBuf,
    line_override: Option<u32>,
    module_depth: usize,
    ctx: &'c mut Ctx<'r>,
}

impl<'a> Reader<'_, 'a, '_, '_> {
    fn line(&self, offset: u32) -> u32 {
        self.line_override.unwrap_or_else(|| self.lines.line(offset as usize))
    }

    /// The value of package import `source`, or of the project module it
    /// names, as `name` (`None` for its default export).
    fn import(&mut self, source: &str, name: Option<&str>, line: u32) -> Val {
        let resolved = self.ctx.resolver.and_then(|r| r.resolve(&self.dir, source));
        if let Some(file) = resolved {
            let over = Some(self.line_override.unwrap_or(line));
            return module_export(self.ctx, &file, name, over, self.module_depth + 1)
                .unwrap_or(Val::Unknown);
        }
        if source.starts_with('.') {
            return Val::Unknown;
        }
        Val::Import { source: source.to_string(), path: name.unwrap_or_default().to_string() }
    }

    fn value(&mut self, expr: &Expression<'a>, depth: usize) -> Val {
        if depth > MAX_DEPTH {
            return Val::Unknown;
        }
        let line = self.line(expr.span().start);
        match expr.get_inner_expression() {
            Expression::StringLiteral(s) => Val::Str(s.value.to_string()),
            Expression::TemplateLiteral(t) => {
                t.single_quasi().map_or(Val::Unknown, |s| Val::Str(s.to_string()))
            }
            Expression::BooleanLiteral(b) => Val::Bool(b.value),
            Expression::NumericLiteral(n) => Val::Num(n.value),
            Expression::NullLiteral(_) => Val::Null,
            Expression::Identifier(id) if id.name == "undefined" => Val::Null,
            Expression::ArrayExpression(a) => {
                let mut items = Vec::new();
                for el in &a.elements {
                    match el {
                        ArrayExpressionElement::SpreadElement(s) => items.push(Item {
                            val: self.value(&s.argument, depth + 1),
                            line: self.line(s.span.start),
                            spread: true,
                        }),
                        ArrayExpressionElement::Elision(_) => {}
                        e => {
                            if let Some(e) = e.as_expression() {
                                items.push(Item {
                                    val: self.value(e, depth + 1),
                                    line: self.line(e.span().start),
                                    spread: false,
                                });
                            }
                        }
                    }
                }
                Val::Arr(items)
            }
            Expression::ObjectExpression(o) => {
                let mut props = Vec::new();
                for p in &o.properties {
                    match p {
                        ObjectPropertyKind::ObjectProperty(p) => {
                            let key =
                                p.key.static_name().map_or_else(|| "?".to_string(), |k| k.into());
                            props.push(Prop {
                                key,
                                val: self.value(&p.value, depth + 1),
                                line: self.line(p.span.start),
                            });
                        }
                        ObjectPropertyKind::SpreadProperty(s) => {
                            let line = self.line(s.span.start);
                            match self.value(&s.argument, depth + 1) {
                                Val::Obj(base) => props.extend(base),
                                v => props.push(Prop { key: SPREAD.to_string(), val: v, line }),
                            }
                        }
                    }
                }
                Val::Obj(props)
            }
            Expression::CallExpression(c) if c.is_require_call() => match c.arguments.first() {
                Some(Argument::StringLiteral(s)) => self.import(&s.value, None, line),
                _ => Val::Unknown,
            },
            Expression::CallExpression(c) => {
                let (callee, source) = self.callee(&c.callee);
                let args = self.arguments(&c.arguments, depth);
                Val::Call { callee, source, args }
            }
            Expression::NewExpression(n) => {
                let (callee, source) = self.callee(&n.callee);
                let args = self.arguments(&n.arguments, depth);
                Val::Call { callee: format!("new {callee}"), source, args }
            }
            Expression::StaticMemberExpression(m) => {
                let object = self.value(&m.object, depth + 1);
                member(object, m.property.name.as_str())
            }
            Expression::ComputedMemberExpression(m) => match m.expression.get_inner_expression() {
                Expression::StringLiteral(s) => {
                    let object = self.value(&m.object, depth + 1);
                    member(object, &s.value)
                }
                _ => Val::Unknown,
            },
            Expression::Identifier(id) => {
                let name = id.name.as_str();
                if let Some(e) = self.consts.get(name).copied() {
                    return self.value(e, depth + 1);
                }
                match self.imports.get(name).cloned() {
                    Some((source, imported)) => self.import(&source, imported.as_deref(), line),
                    None => Val::Unknown,
                }
            }
            Expression::ConditionalExpression(c) => Val::Either(
                Box::new(self.value(&c.consequent, depth + 1)),
                Box::new(self.value(&c.alternate, depth + 1)),
            ),
            Expression::LogicalExpression(l) => Val::Either(
                Box::new(self.value(&l.left, depth + 1)),
                Box::new(self.value(&l.right, depth + 1)),
            ),
            Expression::AwaitExpression(a) => self.value(&a.argument, depth + 1),
            _ => Val::Unknown,
        }
    }

    fn arguments(&mut self, args: &[Argument<'a>], depth: usize) -> Vec<Item> {
        args.iter()
            .filter_map(|a| match a {
                Argument::SpreadElement(s) => Some(Item {
                    val: self.value(&s.argument, depth + 1),
                    line: self.line(s.span.start),
                    spread: true,
                }),
                a => a.as_expression().map(|e| Item {
                    val: self.value(e, depth + 1),
                    line: self.line(e.span().start),
                    spread: false,
                }),
            })
            .collect()
    }

    /// A callee as written, as `tseslint.config`, and the package its
    /// first name is imported from.
    fn callee(&self, e: &Expression<'a>) -> (String, Option<String>) {
        let mut parts = Vec::new();
        let mut cur = e.get_inner_expression();
        loop {
            match cur {
                Expression::StaticMemberExpression(m) => {
                    parts.push(m.property.name.to_string());
                    cur = m.object.get_inner_expression();
                }
                Expression::Identifier(id) => {
                    parts.push(id.name.to_string());
                    parts.reverse();
                    let source = self.imports.get(id.name.as_str()).map(|(s, _)| s.clone());
                    return (parts.join("."), source);
                }
                _ => return (String::new(), None),
            }
        }
    }
}

/// Property `key` of `object`.
fn member(object: Val, key: &str) -> Val {
    match object {
        Val::Import { source, path } => {
            let path = if path.is_empty() { key.to_string() } else { format!("{path}.{key}") };
            Val::Import { source, path }
        }
        Val::Obj(_) => object.get(key).cloned().unwrap_or(Val::Unknown),
        _ => Val::Unknown,
    }
}

/// Where `--fix` can add global ignores to a flat config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FixSite {
    /// A new `{ ignores: [...] }` goes first: `expected` (the opening
    /// bracket or parenthesis) at `start` is followed by it, on a line of
    /// its own with `indent` when the first element has one.
    Insert { start: usize, expected: String, indent: Option<String>, empty: bool },
    /// Patterns go after `expected`, the last pattern of an existing
    /// global ignores array at `start`, or its `[` when it is empty.
    Append { start: usize, expected: String, empty: bool },
}

/// Where the flat config `src` of `file` can take global ignores, and its
/// quote character, when its export is an array (or a call of
/// `defineConfig`/`tseslint.config`) written in the file.
pub(crate) fn flat_fix_site(file: &Path, src: &str) -> Option<(FixSite, char)> {
    let source_type = SourceType::from_path(file).ok()?;
    let allocator = Allocator::default();
    let ret = Parser::new(&allocator, src, source_type).parse();
    if !ret.errors.is_empty() {
        return None;
    }
    let mut consts: HashMap<&str, &Expression<'_>> = HashMap::new();
    let mut exported = None;
    for stmt in &ret.program.body {
        match stmt {
            Statement::VariableDeclaration(decl) => declare(decl, &mut consts),
            Statement::ExportDefaultDeclaration(decl) => {
                exported = decl.declaration.as_expression()
            }
            Statement::ExpressionStatement(s) => {
                if let Expression::AssignmentExpression(a) = &s.expression
                    && a.left
                        .as_member_expression()
                        .is_some_and(|m| m.is_specific_member_access("module", "exports"))
                {
                    exported = Some(&a.right);
                }
            }
            _ => {}
        }
    }
    let mut e = exported?.get_inner_expression();
    if let Expression::Identifier(id) = e {
        e = consts.get(id.name.as_str())?.get_inner_expression();
    }
    let quote = if src.matches('\'').count() > src.matches('"').count() { '\'' } else { '"' };
    // The elements, and the offset of the bracket or parenthesis before them.
    let (elements, open): (Vec<(Span, Option<&Expression<'_>>)>, usize) = match e {
        Expression::ArrayExpression(a) => (array_elements(a)?, a.span.start as usize),
        Expression::CallExpression(c) if is_config_helper(&c.callee) => {
            let single = match c.arguments.as_slice() {
                [a] => a.as_expression().map(Expression::get_inner_expression),
                _ => None,
            };
            if let Some(Expression::ArrayExpression(a)) = single {
                (array_elements(a)?, a.span.start as usize)
            } else {
                let out = c
                    .arguments
                    .iter()
                    .map(|a| match a {
                        Argument::SpreadElement(s) => (s.span, None),
                        a => (a.span(), a.as_expression()),
                    })
                    .collect();
                let after = c.callee.span().end as usize;
                let open = after + src.get(after..)?.find('(')?;
                (out, open)
            }
        }
        _ => return None,
    };
    for el in elements.iter().filter_map(|(_, e)| *e) {
        if let Some(arr) = global_ignores_array(el) {
            let site = match arr.elements.last() {
                Some(last) => {
                    let span = last.span();
                    FixSite::Append {
                        start: span.start as usize,
                        expected: src.get(span.start as usize..span.end as usize)?.to_string(),
                        empty: false,
                    }
                }
                None => FixSite::Append {
                    start: arr.span.start as usize,
                    expected: "[".to_string(),
                    empty: true,
                },
            };
            return Some((site, quote));
        }
    }
    let expected = src.get(open..=open)?.to_string();
    let indent = elements.first().and_then(|(span, _)| {
        let start = span.start as usize;
        let line_start = src[..start].rfind('\n').map_or(0, |i| i + 1);
        let before = &src[line_start..start];
        (line_start > open && before.chars().all(|c| c == ' ' || c == '\t'))
            .then(|| before.to_string())
    });
    Some((FixSite::Insert { start: open, expected, indent, empty: elements.is_empty() }, quote))
}

/// The elements of `a`, each with its expression unless it is spread, or
/// `None` when it has holes.
fn array_elements<'s, 'a>(
    a: &'s oxc_ast::ast::ArrayExpression<'a>,
) -> Option<Vec<(Span, Option<&'s Expression<'a>>)>> {
    a.elements
        .iter()
        .map(|el| match el {
            ArrayExpressionElement::SpreadElement(s) => Some((s.span, None)),
            ArrayExpressionElement::Elision(_) => None,
            e => e.as_expression().map(|x| (x.span(), Some(x))),
        })
        .collect()
}

/// Whether `callee` is a helper that returns its arguments as a flat
/// config array: `defineConfig` or typescript-eslint's `config`.
pub(crate) fn is_config_helper(callee: &Expression<'_>) -> bool {
    let name = match callee.get_inner_expression() {
        Expression::Identifier(id) => id.name.as_str(),
        Expression::StaticMemberExpression(m) => m.property.name.as_str(),
        _ => return false,
    };
    matches!(name, "defineConfig" | "config" | "defineFlatConfig")
}

/// The array of string patterns of a global ignores element:
/// `{ ignores: [...] }` (with at most a `name` besides) or
/// `globalIgnores([...])`.
fn global_ignores_array<'s, 'a>(
    e: &'s Expression<'a>,
) -> Option<&'s oxc_ast::ast::ArrayExpression<'a>> {
    let arr = match e.get_inner_expression() {
        Expression::ObjectExpression(o) => {
            let mut ignores = None;
            for p in &o.properties {
                let ObjectPropertyKind::ObjectProperty(p) = p else { return None };
                match p.key.static_name()?.as_ref() {
                    "ignores" => ignores = Some(&p.value),
                    "name" => {}
                    _ => return None,
                }
            }
            ignores?.get_inner_expression()
        }
        Expression::CallExpression(c) => {
            let Expression::Identifier(id) = c.callee.get_inner_expression() else { return None };
            if id.name != "globalIgnores" {
                return None;
            }
            c.arguments.first()?.as_expression()?.get_inner_expression()
        }
        _ => return None,
    };
    match arr {
        Expression::ArrayExpression(a)
            if a.elements
                .iter()
                .all(|el| matches!(el.as_expression(), Some(Expression::StringLiteral(_)))) =>
        {
            Some(a)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(src: &str, name: &str) -> Val {
        let t = tempfile::TempDir::new().unwrap();
        let file = t.path().join(name);
        fs::write(&file, src).unwrap();
        let project = Project::new(t.path());
        load(&file, &Resolver { project: &project }).value.unwrap()
    }

    #[test]
    fn reads_json_with_comments_and_lines() {
        let v =
            json_value("{\n  // c\n  \"a\": [1, \"x\",],\n  \"b\": { \"c\": true }\n}").unwrap();
        assert_eq!(v.prop("a").unwrap().line, 3);
        assert_eq!(v.at(&["b", "c"]).unwrap().val, Val::Bool(true));
        assert!(json_value("{ \"a\": x }").is_none());
        assert!(json_value("{ \"a\": ").is_none());
    }

    #[test]
    fn reads_flat_configs() {
        let v = read(
            "import tseslint from 'typescript-eslint';\nconst on = process.env.CI;\nconst base = { files: ['**/*.ts'] };\nexport default tseslint.config(\n  { ignores: ['dist'] },\n  ...tseslint.configs.recommendedTypeChecked,\n  { ...base, languageOptions: { parserOptions: { projectService: on } } },\n);\n",
            "eslint.config.mjs",
        );
        let Val::Call { callee, source, args } = v else { panic!("{v:?}") };
        assert_eq!(callee, "tseslint.config");
        assert_eq!(source.as_deref(), Some("typescript-eslint"));
        assert_eq!(args[1].line, 6);
        assert!(args[1].spread);
        assert_eq!(
            args[1].val,
            Val::Import {
                source: "typescript-eslint".into(),
                path: "configs.recommendedTypeChecked".into()
            }
        );
        assert_eq!(args[2].val.get("files").unwrap().strings().unwrap(), ["**/*.ts"]);
        let ps = args[2].val.at(&["languageOptions", "parserOptions", "projectService"]).unwrap();
        assert_eq!(ps.val.truthy(), None);
    }

    #[test]
    fn follows_relative_imports_with_the_importing_line() {
        let t = tempfile::TempDir::new().unwrap();
        fs::write(t.path().join("base.js"), "export const shared = [{ ignores: ['out/**'] }];\n")
            .unwrap();
        fs::write(
            t.path().join("eslint.config.js"),
            "import { shared } from './base.js';\n\nexport default [...shared, {}];\n",
        )
        .unwrap();
        let project = Project::new(t.path());
        let v = load(&t.path().join("eslint.config.js"), &Resolver { project: &project }).value;
        let Some(Val::Arr(items)) = v else { panic!() };
        let Val::Arr(inner) = &items[0].val else { panic!() };
        assert_eq!(inner[0].val.get("ignores").unwrap().strings().unwrap(), ["out/**"]);
        assert_eq!(inner[0].line, 3);
    }

    #[test]
    fn finds_where_to_add_ignores() {
        let site = |src: &str| flat_fix_site(Path::new("eslint.config.js"), src).map(|s| s.0);
        assert_eq!(
            site("export default [\n  { rules: {} },\n];\n"),
            Some(FixSite::Insert {
                start: 15,
                expected: "[".into(),
                indent: Some("  ".into()),
                empty: false
            })
        );
        assert_eq!(
            site("export default [{ name: 'x', ignores: ['a'] }, {}];"),
            Some(FixSite::Append { start: 39, expected: "'a'".into(), empty: false })
        );
        assert!(matches!(
            site("export default defineConfig(globalIgnores([]), {});"),
            Some(FixSite::Append { empty: true, .. })
        ));
        assert!(matches!(
            site("const c = tseslint.config({});\nexport default c;"),
            Some(FixSite::Insert { .. })
        ));
        assert_eq!(site("export default antfu({});"), None);
        assert!(matches!(site("export default [...a, {}];"), Some(FixSite::Insert { .. })));
        assert_eq!(site("export default [, {}];"), None);
    }
}
