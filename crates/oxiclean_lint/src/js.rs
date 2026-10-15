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

/// How deeply brackets, braces and parentheses may nest in a module that
/// is read: oxc's parser recurses, and would overflow the stack on a
/// hostile file nested thousands deep.
const MAX_SOURCE_NESTING: usize = 256;

/// How many imports deep other modules are read.
const MAX_MODULE_DEPTH: usize = 4;

/// How many values a config may be read into, all told, before the rest
/// is unknown: constants that name each other twice over grow
/// exponentially.
const MAX_NODES: usize = 200_000;

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
    /// first name comes from, and the arguments. A method called on a
    /// constant made with `new`, as `compat.config(...)` after
    /// `const compat = new FlatCompat()`, has the class's name in
    /// `instance_of` and the package it comes from in `source`.
    Call {
        callee: String,
        source: Option<String>,
        instance_of: Option<String>,
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
/// value has a line. Comments and trailing commas are allowed. `None`
/// when it isn't one.
pub(crate) fn json_value(src: &str) -> Option<Val> {
    let src = src.trim_start_matches('\u{feff}');
    // Values nested deeper than `MAX_DEPTH` aren't plain data once read,
    // and oxc's recursive parser would overflow the stack on a hostile
    // file long before its depth matters.
    if nests_deeper_than(src, MAX_DEPTH + 1) {
        return None;
    }
    // `(` keeps every line where it was.
    let wrapped = format!("({src}\n)");
    let allocator = Allocator::default();
    let ret = Parser::new(&allocator, &wrapped, SourceType::mjs()).parse();
    if !ret.errors.is_empty() {
        return None;
    }
    let [Statement::ExpressionStatement(s)] = ret.program.body.as_slice() else { return None };
    let mut ctx = Ctx::new(None);
    let mut reader = Reader::new(&mut ctx, &wrapped, Module::default(), PathBuf::new(), None, 0);
    let v = reader.value(&s.expression, 0);
    // Only plain data counts as JSON.
    plain(&v).then_some(v)
}

/// As [`json_value`], for a document that must be strict JSON, comments
/// aside, as ESLint 8 reads `.eslintrc.json` and package.json: a trailing
/// comma makes it unreadable.
pub(crate) fn strict_json_value(src: &str) -> Option<Val> {
    let src = src.trim_start_matches('\u{feff}');
    serde_json::from_str::<Value>(&without_comments(src)).ok()?;
    json_value(src)
}

/// Whether brackets and braces in `src`, outside strings and comments,
/// nest more than `max` deep.
fn nests_deeper_than(src: &str, max: usize) -> bool {
    let mut depth = 0usize;
    let mut quote = None;
    let mut chars = src.chars().peekable();
    while let Some(c) = chars.next() {
        if let Some(q) = quote {
            match c {
                '\\' => {
                    chars.next();
                }
                _ if c == q => quote = None,
                _ => {}
            }
            continue;
        }
        match (c, chars.peek()) {
            ('"' | '\'' | '`', _) => quote = Some(c),
            ('/', Some('/')) => while chars.next_if(|&n| n != '\n').is_some() {},
            ('/', Some('*')) => {
                chars.next();
                let mut prev = ' ';
                for n in chars.by_ref() {
                    if prev == '*' && n == '/' {
                        break;
                    }
                    prev = n;
                }
            }
            ('[' | '{' | '(', _) => {
                depth += 1;
                if depth > max {
                    return true;
                }
            }
            (']' | '}' | ')', _) => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    false
}

/// `src` with its `//` and `/* */` comments blanked out, strings kept.
fn without_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut chars = src.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            match c {
                '\\' => out.extend(chars.next()),
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match (c, chars.peek()) {
            ('"', _) => {
                in_string = true;
                out.push(c);
            }
            ('/', Some('/')) => while chars.next_if(|&n| n != '\n').is_some() {},
            ('/', Some('*')) => {
                chars.next();
                let mut prev = ' ';
                for n in chars.by_ref() {
                    if prev == '*' && n == '/' {
                        break;
                    }
                    prev = n;
                }
                out.push(' ');
            }
            _ => out.push(c),
        }
    }
    out
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
    let mut ctx = Ctx::new(Some(resolver));
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
    /// How many more values may be read, of [`MAX_NODES`].
    budget: usize,
}

impl<'r> Ctx<'r> {
    fn new(resolver: Option<&'r Resolver<'r>>) -> Self {
        Self { resolver, loaded: Loaded::default(), visiting: Vec::new(), budget: MAX_NODES }
    }
}

/// What a module exports as `name`, or by default. `line` replaces the
/// lines of everything read from another module with the line that
/// imports it. A module that doesn't parse can't be read.
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
    if nests_deeper_than(&src, MAX_SOURCE_NESTING) {
        return None;
    }
    let allocator = Allocator::default();
    let ret = Parser::new(&allocator, &src, source_type).parse();
    if ret.panicked || !ret.errors.is_empty() {
        return None;
    }
    ctx.visiting.push(file.to_path_buf());
    let dir = file.parent().map(Path::to_path_buf).unwrap_or_default();
    let module = Module::collect(&ret.program, &mut ctx.loaded.packages);
    let mut reader = Reader::new(ctx, &src, module, dir, line, module_depth);
    let out = reader.export(name);
    reader.ctx.visiting.pop();
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

/// The top-level declarations of a module that a config is read from.
#[derive(Default)]
struct Module<'s, 'a> {
    consts: HashMap<&'s str, &'s Expression<'a>>,
    /// Local name → package or path, and the name imported (`None` for
    /// the default or namespace).
    imports: HashMap<&'s str, (String, Option<String>)>,
    /// `export default` or `module.exports =`.
    exported: Option<&'s Expression<'a>>,
    /// `export const x = ...`.
    named: HashMap<String, &'s Expression<'a>>,
    /// `export { a as b }`: b → a.
    aliases: Vec<(String, String)>,
}

impl<'s, 'a> Module<'s, 'a> {
    /// Reads the declarations of `program`, adding every package it
    /// imports or requires to `packages`.
    fn collect(program: &'s Program<'a>, packages: &mut Vec<String>) -> Self {
        let mut m = Self::default();
        for stmt in &program.body {
            match stmt {
                Statement::ImportDeclaration(decl) => {
                    let source = decl.source.value.to_string();
                    packages.push(source.clone());
                    for spec in decl.specifiers.iter().flatten() {
                        let (local, imported) = import_specifier(spec);
                        m.imports.insert(local, (source.clone(), imported));
                    }
                }
                Statement::VariableDeclaration(decl) => declare(decl, &mut m.consts),
                Statement::ExportDefaultDeclaration(decl) => {
                    m.exported = decl.declaration.as_expression();
                }
                Statement::ExportNamedDeclaration(decl) => {
                    if let Some(Declaration::VariableDeclaration(v)) = &decl.declaration {
                        declare(v, &mut m.consts);
                        for d in &v.declarations {
                            if let (Some(n), Some(init)) = (d.id.get_identifier_name(), &d.init) {
                                m.named.insert(n.to_string(), init);
                            }
                        }
                    }
                    for s in &decl.specifiers {
                        m.aliases.push((s.exported.name().to_string(), s.local.name().to_string()));
                    }
                }
                Statement::ExpressionStatement(s) => {
                    if let Some(right) = module_exports_assignment(&s.expression) {
                        m.exported = Some(right);
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
                m.consts.remove(name);
            }
        }
        // `require('x')` imports too.
        for e in m.consts.values() {
            if let Expression::CallExpression(c) = e.get_inner_expression()
                && c.is_require_call()
                && let Some(Argument::StringLiteral(s)) = c.arguments.first()
            {
                packages.push(s.value.to_string());
            }
        }
        m
    }
}

/// The local name an import specifier binds, and the name it imports
/// (`None` for the default export or the namespace).
fn import_specifier<'s>(spec: &'s ImportDeclarationSpecifier<'_>) -> (&'s str, Option<String>) {
    match spec {
        ImportDeclarationSpecifier::ImportSpecifier(s) => {
            let imported = match &s.imported {
                ModuleExportName::IdentifierName(n) => n.name.to_string(),
                ModuleExportName::IdentifierReference(n) => n.name.to_string(),
                ModuleExportName::StringLiteral(n) => n.value.to_string(),
            };
            (s.local.name.as_str(), (imported != "default").then_some(imported))
        }
        ImportDeclarationSpecifier::ImportDefaultSpecifier(s) => (s.local.name.as_str(), None),
        ImportDeclarationSpecifier::ImportNamespaceSpecifier(s) => (s.local.name.as_str(), None),
    }
}

/// The right side of `module.exports = ...`.
fn module_exports_assignment<'s, 'a>(e: &'s Expression<'a>) -> Option<&'s Expression<'a>> {
    match e {
        Expression::AssignmentExpression(a)
            if a.left
                .as_member_expression()
                .is_some_and(|m| m.is_specific_member_access("module", "exports")) =>
        {
            Some(&a.right)
        }
        _ => None,
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
    module: Module<'s, 'a>,
    /// Constants already read, with how many values each took.
    memo: HashMap<&'s str, (Val, usize)>,
    dir: PathBuf,
    line_override: Option<u32>,
    module_depth: usize,
    ctx: &'c mut Ctx<'r>,
}

impl<'s, 'a, 'c, 'r> Reader<'s, 'a, 'c, 'r> {
    fn new(
        ctx: &'c mut Ctx<'r>,
        src: &str,
        module: Module<'s, 'a>,
        dir: PathBuf,
        line_override: Option<u32>,
        module_depth: usize,
    ) -> Self {
        Self {
            lines: LineIndex::new(src),
            module,
            memo: HashMap::new(),
            dir,
            line_override,
            module_depth,
            ctx,
        }
    }

    fn line(&self, offset: u32) -> u32 {
        self.line_override.unwrap_or_else(|| self.lines.line(offset as usize))
    }

    /// What the module exports as `name`, or by default.
    fn export(&mut self, name: Option<&str>) -> Option<Val> {
        let local = |reader: &Self, exported: &str| {
            let (_, local) = reader.module.aliases.iter().find(|(e, _)| e == exported)?;
            reader.module.consts.get(local.as_str()).copied()
        };
        match name {
            None => {
                let exported = self.module.exported.or_else(|| local(self, "default"))?;
                Some(self.value(exported, 0))
            }
            Some(n) => match self.module.named.get(n).copied().or_else(|| local(self, n)) {
                Some(e) => Some(self.value(e, 0)),
                // `module.exports = { n }`.
                None => {
                    let v = self.value(self.module.exported?, 0);
                    v.get(n).cloned()
                }
            },
        }
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
        if depth > MAX_DEPTH || self.ctx.budget == 0 {
            return Val::Unknown;
        }
        self.ctx.budget -= 1;
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
            Expression::ArrayExpression(a) => self.array(a, depth),
            Expression::ObjectExpression(o) => self.object(o, depth),
            Expression::CallExpression(c) if c.is_require_call() => match c.arguments.first() {
                Some(Argument::StringLiteral(s)) => self.import(&s.value, None, line),
                _ => Val::Unknown,
            },
            Expression::CallExpression(c) => {
                let (callee, source, instance_of) = self.callee(&c.callee);
                let args = self.arguments(&c.arguments, depth);
                Val::Call { callee, source, instance_of, args }
            }
            Expression::NewExpression(n) => {
                let (callee, source, instance_of) = self.callee(&n.callee);
                let args = self.arguments(&n.arguments, depth);
                Val::Call { callee: format!("new {callee}"), source, instance_of, args }
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
            Expression::Identifier(id) => self.identifier(id.name.as_str(), line, depth),
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

    /// The value a name stands for: a constant, read once, or an import.
    fn identifier(&mut self, name: &str, line: u32, depth: usize) -> Val {
        if let Some((name, e)) = self.module.consts.get_key_value(name).map(|(k, e)| (*k, *e)) {
            if let Some((v, size)) = self.memo.get(name) {
                if self.ctx.budget < *size {
                    self.ctx.budget = 0;
                    return Val::Unknown;
                }
                self.ctx.budget -= size;
                return v.clone();
            }
            let before = self.ctx.budget;
            let v = self.value(e, depth + 1);
            // A constant cut short by the depth limit is read again where
            // it is shallower.
            if depth < MAX_DEPTH / 2 {
                self.memo.insert(name, (v.clone(), before - self.ctx.budget));
            }
            return v;
        }
        match self.module.imports.get(name).cloned() {
            Some((source, imported)) => self.import(&source, imported.as_deref(), line),
            None => Val::Unknown,
        }
    }

    fn array(&mut self, a: &oxc_ast::ast::ArrayExpression<'a>, depth: usize) -> Val {
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

    fn object(&mut self, o: &oxc_ast::ast::ObjectExpression<'a>, depth: usize) -> Val {
        let mut props = Vec::new();
        for p in &o.properties {
            match p {
                ObjectPropertyKind::ObjectProperty(p) => {
                    let key = p.key.static_name().map_or_else(|| "?".to_string(), |k| k.into());
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

    /// A callee as written, as `tseslint.config`, the package its first
    /// name is imported from, and, when that name is a constant made with
    /// `new X()`, X (whose package is then the source).
    fn callee(&self, e: &Expression<'a>) -> (String, Option<String>, Option<String>) {
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
                    let name = id.name.as_str();
                    if let Some((s, _)) = self.module.imports.get(name) {
                        return (parts.join("."), Some(s.clone()), None);
                    }
                    let class = match self.module.consts.get(name).map(|e| e.get_inner_expression())
                    {
                        Some(Expression::NewExpression(n)) => match n.callee.get_inner_expression()
                        {
                            Expression::Identifier(c) => Some(c.name.as_str()),
                            _ => None,
                        },
                        _ => None,
                    };
                    let source =
                        class.and_then(|c| self.module.imports.get(c)).map(|(s, _)| s.clone());
                    return (parts.join("."), source, class.map(str::to_string));
                }
                _ => return (String::new(), None, None),
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
    /// global ignores array at `start`, or its `[` when it is empty;
    /// `existing` are the patterns it has.
    Append { start: usize, expected: String, empty: bool, existing: Vec<String> },
}

/// The elements of a config array, each with its expression unless it is
/// spread, and the offset of the bracket or parenthesis before them.
type Elements<'s, 'a> = (Vec<(Span, Option<&'s Expression<'a>>)>, usize);

/// Where the flat config `src` of `file` can take global ignores, and its
/// quote character, when its export is an array (or a call of
/// `defineConfig`/`tseslint.config`) written in the file.
pub(crate) fn flat_fix_site(file: &Path, src: &str) -> Option<(FixSite, char)> {
    let source_type = SourceType::from_path(file).ok()?;
    if nests_deeper_than(src, MAX_SOURCE_NESTING) {
        return None;
    }
    let allocator = Allocator::default();
    let ret = Parser::new(&allocator, src, source_type).parse();
    if !ret.errors.is_empty() {
        return None;
    }
    let module = Module::collect(&ret.program, &mut Vec::new());
    let mut e = module.exported?.get_inner_expression();
    if let Expression::Identifier(id) = e {
        e = module.consts.get(id.name.as_str())?.get_inner_expression();
    }
    let quote = if src.matches('\'').count() > src.matches('"').count() { '\'' } else { '"' };
    let (elements, open) = config_elements(e, &module.imports, src)?;
    for el in elements.iter().filter_map(|(_, e)| *e) {
        if let Some(arr) = global_ignores_array(el) {
            return Some((append_site(arr, src)?, quote));
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

/// The elements of the config array `e` exports: an array, or the
/// arguments of a config helper.
fn config_elements<'s, 'a>(
    e: &'s Expression<'a>,
    imports: &HashMap<&str, (String, Option<String>)>,
    src: &str,
) -> Option<Elements<'s, 'a>> {
    match e {
        Expression::ArrayExpression(a) => Some((array_elements(a)?, a.span.start as usize)),
        Expression::CallExpression(c) if is_config_helper(&c.callee, imports) => {
            let single = match c.arguments.as_slice() {
                [a] => a.as_expression().map(Expression::get_inner_expression),
                _ => None,
            };
            if let Some(Expression::ArrayExpression(a)) = single {
                return Some((array_elements(a)?, a.span.start as usize));
            }
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
            Some((out, open))
        }
        _ => None,
    }
}

/// Where patterns go in the global ignores array `arr`.
fn append_site(arr: &oxc_ast::ast::ArrayExpression<'_>, src: &str) -> Option<FixSite> {
    let existing = arr
        .elements
        .iter()
        .filter_map(|el| match el.as_expression() {
            Some(Expression::StringLiteral(s)) => Some(s.value.to_string()),
            _ => None,
        })
        .collect();
    Some(match arr.elements.last() {
        Some(last) => {
            let span = last.span();
            FixSite::Append {
                start: span.start as usize,
                expected: src.get(span.start as usize..span.end as usize)?.to_string(),
                empty: false,
                existing,
            }
        }
        None => FixSite::Append {
            start: arr.span.start as usize,
            expected: "[".to_string(),
            empty: true,
            existing,
        },
    })
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

/// Packages whose `config` export returns its arguments as a flat config
/// array.
const CONFIG_HELPER_PACKAGES: &[&str] = &["typescript-eslint", "eslint/config"];

/// Whether a package's export `name` returns its arguments as a flat
/// config array: `defineConfig` (from `eslint/config` or a package like
/// it), or `config` from typescript-eslint.
pub(crate) fn is_config_helper_export(source: &str, name: &str) -> bool {
    let from_package = !source.starts_with('.') && !source.starts_with('/');
    match name {
        "defineConfig" | "defineFlatConfig" => from_package,
        "config" => CONFIG_HELPER_PACKAGES.contains(&source),
        _ => false,
    }
}

/// Whether `callee` is a config helper imported into the module: a name
/// imported as one, or a property of a package imported whole.
fn is_config_helper(
    callee: &Expression<'_>,
    imports: &HashMap<&str, (String, Option<String>)>,
) -> bool {
    match callee.get_inner_expression() {
        Expression::Identifier(id) => match imports.get(id.name.as_str()) {
            Some((source, Some(imported))) => is_config_helper_export(source, imported),
            _ => false,
        },
        Expression::StaticMemberExpression(m) => match m.object.get_inner_expression() {
            Expression::Identifier(id) => match imports.get(id.name.as_str()) {
                Some((source, None)) => is_config_helper_export(source, &m.property.name),
                _ => false,
            },
            _ => false,
        },
        _ => false,
    }
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
        let Val::Call { callee, source, args, .. } = v else { panic!("{v:?}") };
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
            Some(FixSite::Append {
                start: 39,
                expected: "'a'".into(),
                empty: false,
                existing: vec!["a".into()]
            })
        );
        let define = "import { defineConfig, globalIgnores } from 'eslint/config';\n";
        assert!(matches!(
            site(&format!("{define}export default defineConfig(globalIgnores([]), {{}});")),
            Some(FixSite::Append { empty: true, .. })
        ));
        assert!(matches!(
            site(
                "import tseslint from 'typescript-eslint';\nconst c = tseslint.config({});\nexport default c;"
            ),
            Some(FixSite::Insert { .. })
        ));
        // Not a flat config helper: FlatCompat reads eslintrc objects, and
        // a `config` or `defineConfig` that isn't imported may be anything.
        assert_eq!(
            site(
                "import { FlatCompat } from '@eslint/eslintrc';\nconst compat = new FlatCompat();\nexport default compat.config({ rules: {} });"
            ),
            None
        );
        assert_eq!(site("export default defineConfig({});"), None);
        assert_eq!(site("import x from './x.js';\nexport default x.config({});"), None);
        assert_eq!(site("export default antfu({});"), None);
        assert!(matches!(site("export default [...a, {}];"), Some(FixSite::Insert { .. })));
        assert_eq!(site("export default [, {}];"), None);
    }

    #[test]
    fn deeply_nested_configs_are_unreadable_without_overflowing() {
        // Test threads have small stacks; oxc's parser would overflow on
        // this before the depth limit cut it short.
        let deep = format!("{{ \"a\": {}1{} }}", "[".repeat(100_000), "]".repeat(100_000));
        assert!(json_value(&deep).is_none());
        // Brackets in strings and comments don't count.
        let quoted = format!("{{ \"a\": \"{}\" /* {} */ }}", "[".repeat(1000), "{".repeat(1000));
        assert!(json_value(&quoted).is_some());
        let t = tempfile::TempDir::new().unwrap();
        let file = t.path().join("eslint.config.js");
        let module = format!("export default {}1{};\n", "[".repeat(100_000), "]".repeat(100_000));
        fs::write(&file, &module).unwrap();
        let project = Project::new(t.path());
        assert_eq!(load(&file, &Resolver { project: &project }).value, None);
        assert_eq!(flat_fix_site(&file, &module), None);
        // Just under the limit, the parser still fits the stack.
        let n = MAX_SOURCE_NESTING - 1;
        fs::write(&file, format!("export default {}1{};\n", "[".repeat(n), "]".repeat(n))).unwrap();
        assert!(matches!(load(&file, &Resolver { project: &project }).value, Some(Val::Arr(_))));
        let at_limit = format!("{}1{}", "[".repeat(MAX_DEPTH), "]".repeat(MAX_DEPTH));
        assert!(json_value(&at_limit).is_some());
    }

    #[test]
    fn strict_json_rejects_trailing_commas() {
        assert!(strict_json_value("{ // c\n \"a\": \"//x\" /* y */ }").is_some());
        assert!(strict_json_value("{ \"a\": [1,] }").is_none());
        assert!(json_value("{ \"a\": [1,] }").is_some());
    }

    #[test]
    fn modules_that_dont_parse_are_unreadable() {
        let t = tempfile::TempDir::new().unwrap();
        let file = t.path().join("eslint.config.js");
        fs::write(&file, "export default [{ rules: {} }];\nconst = ;\n").unwrap();
        let project = Project::new(t.path());
        assert_eq!(load(&file, &Resolver { project: &project }).value, None);
    }

    #[test]
    fn constants_named_twice_over_stay_bounded() {
        let mut src = String::new();
        for i in 0..40 {
            src.push_str(&format!("const a{i} = [a{n}, a{n}];\n", n = i + 1));
        }
        src.push_str("const a40 = { rules: {} };\nexport default a0;\n");
        let start = std::time::Instant::now();
        let v = read(&src, "eslint.config.js");
        assert!(matches!(v, Val::Arr(_)));
        assert!(start.elapsed() < std::time::Duration::from_secs(20));
    }
}
