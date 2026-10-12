//! What a module takes from the modules it imports, for unused export checks.
//!
//! Follows knip's reading of imports: a named or default import uses that
//! export whether or not the binding is referenced, type-only imports count
//! like any other, and a namespace import uses the members accessed on it,
//! or every export once the namespace is used as a whole.
//!
//! Members read on an imported binding, as `E.a` or `N.a.b`, are recorded as
//! paths for the checks of enum and namespace members, and so are reads of
//! every member at once: `E[key]`, `Object.keys(E)`, the type `E` and
//! `typeof E`. As in knip, passing `E` around uses none of its members.

use std::collections::{HashMap, HashSet};

use oxc_ast::ast::{
    Argument, BindingPatternKind, CallExpression, ComputedMemberExpression,
    ExportDefaultDeclaration, ExportDefaultDeclarationKind, ExportNamedDeclaration, Expression,
    ExpressionStatement, IdentifierReference, ImportDeclarationSpecifier, ImportExpression,
    JSXMemberExpression, JSXMemberExpressionObject, ObjectPattern, Program, Statement,
    StaticMemberExpression, TSInterfaceDeclaration, TSInterfaceHeritage, TSModuleDeclaration,
    TSModuleDeclarationName, TSModuleReference, TSQualifiedName, TSTypeName, TSTypeQuery,
    TSTypeQueryExprName, TSTypeReference, VariableDeclarator,
};
use oxc_ast_visit::{Visit, walk};

/// What an import takes from the module it loads.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Used {
    /// One export, `default` for the default export.
    Name(String),
    /// A member of an export, as `x.a` on `import { x } from './m'`: the
    /// export `x` is used, and when `x` is a namespace re-exported from
    /// another module, only its member `a` there. `member` is the path
    /// read, so `x.a.b` uses `["a", "b"]`.
    Member { name: String, member: Vec<String> },
    /// An export and every one of its members: `E[key]`, `Object.keys(E)`,
    /// `const e = E`, the type `E` or `typeof E`.
    Whole(String),
    /// Every export: a namespace used as a whole, or a dynamic `import()`
    /// whose result isn't destructured.
    All,
    /// The default export, or every export when there is none: an
    /// `import()` returned by an arrow passed to a call, as in
    /// `lazy(() => import('./page'))`.
    Loader,
}

/// One use of another module's exports.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct Usage {
    /// The module, as written in the import.
    pub request: String,
    pub used: Used,
}

/// What a module takes from other modules, apart from what its own exports
/// pass on (see [`crate::Export::forwarded`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ModuleUses {
    /// Uses of other modules' exports, without duplicates, in source order.
    pub usages: Vec<Usage>,
    /// Modules whose exports, except `default`, the module re-exports with
    /// `export * from`.
    pub reexport_all: Vec<String>,
}

/// Member paths in the order first read, without duplicates.
#[derive(Default)]
struct Paths {
    list: Vec<Vec<String>>,
    seen: HashSet<Vec<String>>,
}

impl Paths {
    fn insert(&mut self, path: &[&str]) {
        let path: Vec<String> = path.iter().map(|p| (*p).to_string()).collect();
        if !self.seen.contains(&path) {
            self.seen.insert(path.clone());
            self.list.push(path);
        }
    }
}

/// An imported binding at the top level, or a local holding an awaited
/// `import()`.
#[derive(Debug)]
struct Binding {
    request: String,
    /// The export imported, or `None` for a namespace.
    name: Option<String>,
}

/// Lists what `program` takes from the modules it imports.
pub(crate) fn collect(program: &Program<'_>) -> ModuleUses {
    let mut c = Collector::default();
    for stmt in &program.body {
        c.declare(stmt);
    }
    c.visit_program(program);
    c.finish()
}

#[derive(Default)]
struct Collector {
    bindings: HashMap<String, Binding>,
    /// Imported bindings exported unchanged with `export { a }` or
    /// `export default a`.
    reexported: HashSet<String>,
    /// References to each binding outside such exports.
    refs: HashMap<String, usize>,
    /// Namespaces used as a whole.
    opaque: HashSet<String>,
    /// Member paths read on each binding, as `[a]` or `[a, b]`.
    members: HashMap<String, Paths>,
    /// Paths on each binding whose every member is read, empty for the
    /// binding itself.
    whole: HashMap<String, Paths>,
    usages: Vec<Usage>,
    seen: HashSet<Usage>,
    reexport_all: Vec<String>,
    /// Starts of `import()` expressions already accounted for.
    handled_imports: HashSet<u32>,
}

impl Collector {
    fn bind(&mut self, local: &str, request: &str, name: Option<&str>) {
        match self.bindings.get(local) {
            None => {
                let binding =
                    Binding { request: request.to_string(), name: name.map(str::to_string) };
                self.bindings.insert(local.to_string(), binding);
            }
            // The same name bound to two modules, as `const mod = await
            // import()` in two functions: which one a use refers to is
            // unknown, so both are used as a whole.
            Some(b) if b.request != request || b.name.as_deref() != name => {
                let other = b.request.clone();
                self.use_(&other, Used::All);
                self.use_(request, Used::All);
            }
            Some(_) => {}
        }
    }

    fn use_(&mut self, request: &str, used: Used) {
        let usage = Usage { request: request.to_string(), used };
        if self.seen.insert(usage.clone()) {
            self.usages.push(usage);
        }
    }

    fn is_bound(&self, name: &str) -> bool {
        self.bindings.contains_key(name)
    }

    fn is_namespace(&self, name: &str) -> bool {
        self.bindings.get(name).is_some_and(|b| b.name.is_none())
    }

    /// Records a read of `path` on the binding `local`.
    fn member(&mut self, local: &str, path: &[&str]) {
        *self.refs.entry(local.to_string()).or_default() += 1;
        self.members.entry(local.to_string()).or_default().insert(path);
    }

    /// Records `E[key]`, `Object.keys(E)` and the like on a member chain: a
    /// read of every member below it. `false`, recording nothing, when the
    /// chain doesn't start from an import, or is a namespace import itself,
    /// which is used as a whole like any other reference to it.
    fn record_whole(&mut self, root: &IdentifierReference<'_>, path: &[&str]) -> bool {
        if !self.is_bound(&root.name) || (path.is_empty() && self.is_namespace(&root.name)) {
            return false;
        }
        *self.refs.entry(root.name.to_string()).or_default() += 1;
        self.whole.entry(root.name.to_string()).or_default().insert(path);
        true
    }

    /// Records the bindings and re-exports of a top-level statement.
    fn declare(&mut self, stmt: &Statement<'_>) {
        match stmt {
            Statement::ImportDeclaration(decl) => {
                let request = decl.source.value.as_str();
                for spec in decl.specifiers.iter().flatten() {
                    let name = match spec {
                        ImportDeclarationSpecifier::ImportSpecifier(s) => Some(s.imported.name()),
                        ImportDeclarationSpecifier::ImportDefaultSpecifier(_) => {
                            Some("default".into())
                        }
                        ImportDeclarationSpecifier::ImportNamespaceSpecifier(_) => None,
                    };
                    self.bind(&spec.local().name, request, name.as_deref());
                }
            }
            Statement::TSImportEqualsDeclaration(decl) => {
                if let TSModuleReference::ExternalModuleReference(ext) = &decl.module_reference {
                    self.bind(&decl.id.name, &ext.expression.value, Some("default"));
                }
            }
            Statement::ExportAllDeclaration(decl) if decl.exported.is_none() => {
                let request = decl.source.value.to_string();
                if !self.reexport_all.contains(&request) {
                    self.reexport_all.push(request);
                }
            }
            Statement::ExportNamedDeclaration(decl)
                if decl.source.is_none() && decl.declaration.is_none() =>
            {
                for spec in &decl.specifiers {
                    self.reexported.insert(spec.local.name().to_string());
                }
            }
            Statement::ExportDefaultDeclaration(decl) => {
                if let ExportDefaultDeclarationKind::Identifier(id) = &decl.declaration {
                    self.reexported.insert(id.name.to_string());
                }
            }
            _ => {}
        }
    }

    fn finish(mut self) -> ModuleUses {
        let mut bindings: Vec<(&String, &Binding)> = self.bindings.iter().collect();
        // Source order is lost in the map; sort for deterministic output.
        bindings.sort_by(|a, b| (&a.1.request, a.0).cmp(&(&b.1.request, b.0)));
        let mut usages = Vec::new();
        for (local, b) in bindings {
            // An import passed on unchanged is used through its re-export.
            if self.reexported.contains(local) && self.refs.get(local).is_none_or(|&n| n == 0) {
                continue;
            }
            let opaque = self.opaque.contains(local);
            let members = self.members.get(local).map_or(&[][..], |p| p.list.as_slice());
            let whole = self.whole.get(local).map_or(&[][..], |p| p.list.as_slice());
            let mut add = |used| usages.push(Usage { request: b.request.clone(), used });
            match &b.name {
                // `x.a` on an imported `x` uses member `a` of that export,
                // which matters when it is a namespace re-exported as `x`,
                // or an enum or namespace whose members are checked.
                Some(name) => {
                    // A deeper path read as a whole, as `x.a[key]`, counts
                    // for all of `x`.
                    if !whole.is_empty() {
                        add(Used::Whole(name.clone()));
                    }
                    for m in members {
                        add(Used::Member { name: name.clone(), member: m.clone() });
                    }
                    if opaque || (members.is_empty() && whole.is_empty()) {
                        add(Used::Name(name.clone()));
                    }
                }
                None if opaque => add(Used::All),
                // `ns.x.a` reads member `a` of the export `x`.
                None => {
                    for m in members {
                        match m.split_first() {
                            Some((name, [])) => add(Used::Name(name.clone())),
                            Some((name, member)) => {
                                add(Used::Member { name: name.clone(), member: member.to_vec() })
                            }
                            None => {}
                        }
                    }
                    for w in whole {
                        if let Some(name) = w.first() {
                            add(Used::Whole(name.clone()));
                        }
                    }
                }
            }
        }
        for u in usages {
            self.use_(&u.request, u.used);
        }
        ModuleUses { usages: self.usages, reexport_all: self.reexport_all }
    }

    /// `{ a, b: c }` as the names `a` and `b`, or `None` when the pattern
    /// has a rest element or a computed key.
    fn pattern_names(pattern: &ObjectPattern<'_>) -> Option<Vec<String>> {
        if pattern.rest.is_some() {
            return None;
        }
        pattern
            .properties
            .iter()
            .map(|p| if p.computed { None } else { p.key.static_name().map(|n| n.to_string()) })
            .collect()
    }
}

/// The module of `require('m')`.
fn require_request<'a>(call: &'a CallExpression<'_>) -> Option<&'a str> {
    match (&call.callee, call.arguments.as_slice()) {
        (Expression::Identifier(callee), [Argument::StringLiteral(s)])
            if callee.name == "require" =>
        {
            Some(s.value.as_str())
        }
        _ => None,
    }
}

/// The module of `require.resolve('m')` or `import.meta.resolve('m')`: a
/// file loaded some other way, such as in a worker or a child process. knip
/// makes it an entry, so all of its exports count as used.
pub(crate) fn resolve_request<'a>(call: &'a CallExpression<'_>) -> Option<&'a str> {
    let Expression::StaticMemberExpression(callee) = &call.callee else { return None };
    let is_resolver = callee.property.name == "resolve"
        && match &callee.object {
            Expression::Identifier(id) => id.name == "require",
            Expression::MetaProperty(meta) => {
                meta.meta.name == "import" && meta.property.name == "meta"
            }
            _ => false,
        };
    match call.arguments.first() {
        Some(Argument::StringLiteral(s)) if is_resolver => Some(s.value.as_str()),
        _ => None,
    }
}

/// The key of `obj[key]` when it names a member: a string that doesn't read
/// as a number. Any other key may read any member, as `E[code]`, or reads
/// the reverse mapping of a numeric enum, as `E[3]` or `E['3']`.
pub(crate) fn member_key<'b>(key: &'b Expression<'_>) -> Option<&'b str> {
    match key {
        Expression::StringLiteral(s) if !is_numeric(&s.value) => Some(s.value.as_str()),
        _ => None,
    }
}

/// Whether JavaScript reads `key` as a number: `Number(key)` is finite and
/// prints back as `key`, so `'2000'` does but `'0x10'` and `'1e3'` don't.
fn is_numeric(key: &str) -> bool {
    key.parse::<f64>().is_ok_and(|n| n.is_finite() && n.to_string() == key)
}

/// `a.b.c`, or `a['b'].c` with a member key: the identifier it starts from
/// and the names after it. `a` alone is `a` with no names.
pub(crate) fn member_chain<'b, 'a>(
    expr: &'b Expression<'a>,
) -> Option<(&'b IdentifierReference<'a>, Vec<&'b str>)> {
    match expr {
        Expression::Identifier(id) => Some((id, Vec::new())),
        Expression::StaticMemberExpression(m) => {
            let (root, mut path) = member_chain(&m.object)?;
            path.push(m.property.name.as_str());
            Some((root, path))
        }
        Expression::ComputedMemberExpression(m) => {
            let key = member_key(&m.expression)?;
            let (root, mut path) = member_chain(&m.object)?;
            path.push(key);
            Some((root, path))
        }
        _ => None,
    }
}

/// `A.b.c` in a type: the identifier it starts from and the names after it.
pub(crate) fn qualified_chain<'b, 'a>(
    name: &'b TSQualifiedName<'a>,
) -> Option<(&'b IdentifierReference<'a>, Vec<&'b str>)> {
    let (root, mut path) = match &name.left {
        TSTypeName::IdentifierReference(id) => (&**id, Vec::new()),
        TSTypeName::QualifiedName(inner) => qualified_chain(inner)?,
        TSTypeName::ThisExpression(_) => return None,
    };
    path.push(name.right.name.as_str());
    Some((root, path))
}

/// Whether `call` lists every key of its arguments: `Object.keys`,
/// `Object.values`, `Object.entries` or `Object.getOwnPropertyNames`.
pub(crate) fn enumerates(call: &CallExpression<'_>) -> bool {
    let Expression::StaticMemberExpression(callee) = &call.callee else { return false };
    matches!(&callee.object, Expression::Identifier(id) if id.name == "Object")
        && matches!(
            callee.property.name.as_str(),
            "keys" | "values" | "entries" | "getOwnPropertyNames"
        )
}

/// The `import()` an arrow's body returns: `import('m')` or
/// `await import('m')`.
fn loaded<'a, 'b>(expr: &'a Expression<'b>) -> Option<&'a ImportExpression<'b>> {
    match expr.without_parentheses() {
        Expression::ImportExpression(imp) => Some(imp),
        Expression::AwaitExpression(a) => match a.argument.without_parentheses() {
            Expression::ImportExpression(imp) => Some(imp),
            _ => None,
        },
        _ => None,
    }
}

/// The module of `import('m')` with a literal module.
fn import_request<'a>(expr: &'a ImportExpression<'_>) -> Option<&'a str> {
    match &expr.source {
        Expression::StringLiteral(s) => Some(s.value.as_str()),
        Expression::TemplateLiteral(t) if t.expressions.is_empty() && t.quasis.len() == 1 => {
            t.quasis[0].value.cooked.as_ref().map(|c| c.as_str())
        }
        _ => None,
    }
}

impl<'a> Visit<'a> for Collector {
    fn visit_identifier_reference(&mut self, it: &IdentifierReference<'a>) {
        let name = it.name.as_str();
        if self.is_bound(name) {
            *self.refs.entry(name.to_string()).or_default() += 1;
            self.opaque.insert(name.to_string());
        }
    }

    // `export { a }` and `export default a` pass `a` on rather than use it.
    fn visit_export_named_declaration(&mut self, it: &ExportNamedDeclaration<'a>) {
        if it.declaration.is_some() {
            walk::walk_export_named_declaration(self, it);
        }
    }

    fn visit_export_default_declaration(&mut self, it: &ExportDefaultDeclaration<'a>) {
        if !matches!(it.declaration, ExportDefaultDeclarationKind::Identifier(_)) {
            walk::walk_export_default_declaration(self, it);
        }
    }

    // `declare module './x' { ... }` augments `./x`: the names it refers to
    // in types, and the interfaces it extends, are used.
    fn visit_ts_module_declaration(&mut self, it: &TSModuleDeclaration<'a>) {
        if let TSModuleDeclarationName::StringLiteral(module) = &it.id {
            let mut names = AugmentedNames::default();
            if let Some(body) = &it.body {
                names.visit_ts_module_declaration_body(body);
            }
            for name in names.0 {
                self.use_(&module.value, Used::Name(name));
            }
            // Imports referred to inside, such as `ns.Payload`, are used too.
        }
        walk::walk_ts_module_declaration(self, it);
    }

    // A bare `ns;` statement uses nothing, as knip reads it.
    fn visit_expression_statement(&mut self, it: &ExpressionStatement<'a>) {
        if !matches!(it.expression, Expression::Identifier(_)) {
            walk::walk_expression_statement(self, it);
        }
    }

    fn visit_static_member_expression(&mut self, it: &StaticMemberExpression<'a>) {
        if let Some((root, mut path)) = member_chain(&it.object)
            && self.is_bound(&root.name)
        {
            path.push(&it.property.name);
            self.member(&root.name, &path);
            return;
        }
        walk::walk_static_member_expression(self, it);
    }

    fn visit_computed_member_expression(&mut self, it: &ComputedMemberExpression<'a>) {
        if let Some((root, mut path)) = member_chain(&it.object)
            && self.is_bound(&root.name)
        {
            match member_key(&it.expression) {
                Some(key) => {
                    path.push(key);
                    self.member(&root.name, &path);
                    return;
                }
                // `E[code]` may read any member.
                None if self.record_whole(root, &path) => {
                    return self.visit_expression(&it.expression);
                }
                None => {}
            }
        }
        walk::walk_computed_member_expression(self, it);
    }

    fn visit_ts_qualified_name(&mut self, it: &TSQualifiedName<'a>) {
        if let Some((root, path)) = qualified_chain(it)
            && self.is_bound(&root.name)
        {
            self.member(&root.name, &path);
            return;
        }
        walk::walk_ts_qualified_name(self, it);
    }

    // The type `E` stands for any of its members.
    fn visit_ts_type_reference(&mut self, it: &TSTypeReference<'a>) {
        if let TSTypeName::IdentifierReference(id) = &it.type_name
            && self.record_whole(id, &[])
        {
            if let Some(args) = &it.type_arguments {
                self.visit_ts_type_parameter_instantiation(args);
            }
            return;
        }
        walk::walk_ts_type_reference(self, it);
    }

    fn visit_ts_type_query(&mut self, it: &TSTypeQuery<'a>) {
        if let TSTypeQueryExprName::IdentifierReference(id) = &it.expr_name
            && self.record_whole(id, &[])
        {
            if let Some(args) = &it.type_arguments {
                self.visit_ts_type_parameter_instantiation(args);
            }
            return;
        }
        walk::walk_ts_type_query(self, it);
    }

    fn visit_jsx_member_expression(&mut self, it: &JSXMemberExpression<'a>) {
        if let JSXMemberExpressionObject::IdentifierReference(obj) = &it.object
            && self.is_bound(&obj.name)
        {
            self.member(&obj.name, &[&it.property.name]);
            return;
        }
        walk::walk_jsx_member_expression(self, it);
    }

    fn visit_variable_declarator(&mut self, it: &VariableDeclarator<'a>) {
        let Some(init) = &it.init else {
            return walk::walk_variable_declarator(self, it);
        };
        let init = init.without_parentheses();
        let names = match &it.id.kind {
            BindingPatternKind::ObjectPattern(p) => Self::pattern_names(p),
            _ => None,
        };
        match init {
            // `const { a } = ns`
            Expression::Identifier(ns) if self.is_bound(&ns.name) => {
                if let Some(names) = names {
                    for n in names {
                        self.member(&ns.name, &[&n]);
                    }
                    return self.visit_binding_pattern(&it.id);
                }
                // `const e = E` may read any member through `e`.
                if it.id.kind.is_binding_identifier() && self.record_whole(ns, &[]) {
                    return self.visit_binding_pattern(&it.id);
                }
            }
            // `const { a } = require('m')` or `const m = require('m')`
            Expression::CallExpression(call) => {
                if let Some(request) = require_request(call) {
                    match (&it.id.kind, names) {
                        (_, Some(names)) => {
                            for n in names {
                                self.use_(request, Used::Name(n));
                            }
                        }
                        // knip counts this as importing `default`; members
                        // read from it are used too.
                        (BindingPatternKind::BindingIdentifier(id), None) => {
                            self.use_(request, Used::Name("default".into()));
                            self.bind(&id.name, request, None);
                        }
                        _ => self.use_(request, Used::All),
                    }
                    return self.visit_binding_pattern(&it.id);
                }
            }
            // `const { a } = await import('m')` or `const m = await import('m')`
            Expression::AwaitExpression(a) => {
                if let Expression::ImportExpression(imp) = a.argument.without_parentheses()
                    && let Some(request) = import_request(imp)
                {
                    match (&it.id.kind, names) {
                        (_, Some(names)) => {
                            for n in names {
                                self.use_(request, Used::Name(n));
                            }
                        }
                        // knip counts this as importing `default` too.
                        (BindingPatternKind::BindingIdentifier(id), None) => {
                            self.use_(request, Used::Name("default".into()));
                            self.bind(&id.name, request, None);
                        }
                        _ => self.use_(request, Used::All),
                    }
                    self.handled_imports.insert(imp.span.start);
                    return self.visit_binding_pattern(&it.id);
                }
            }
            _ => {}
        }
        walk::walk_variable_declarator(self, it);
    }

    fn visit_call_expression(&mut self, it: &CallExpression<'a>) {
        // A bare `require('m')` takes the default export, as knip reads it.
        if let Some(request) = require_request(it) {
            self.use_(request, Used::Name("default".into()));
            return;
        }
        if let Some(request) = resolve_request(it) {
            self.use_(request, Used::All);
            return;
        }
        // `Object.keys(E)` reads every member of `E`.
        if enumerates(it) {
            for arg in &it.arguments {
                let handled = arg
                    .as_expression()
                    .and_then(member_chain)
                    .is_some_and(|(root, path)| self.record_whole(root, &path));
                if !handled {
                    self.visit_argument(arg);
                }
            }
            return;
        }
        for arg in &it.arguments {
            if let Argument::ArrowFunctionExpression(arrow) = arg
                && arrow.expression
                && let Some(Statement::ExpressionStatement(stmt)) = arrow.body.statements.first()
                && let Some(imp) = loaded(&stmt.expression)
                && let Some(request) = import_request(imp)
            {
                self.use_(request, Used::Loader);
                self.handled_imports.insert(imp.span.start);
            }
        }
        walk::walk_call_expression(self, it);
    }

    fn visit_import_expression(&mut self, it: &ImportExpression<'a>) {
        if !self.handled_imports.contains(&it.span.start)
            && let Some(request) = import_request(it)
        {
            self.use_(request, Used::All);
        }
        walk::walk_import_expression(self, it);
    }
}

/// The names a module augmentation refers to: type references and the
/// interfaces it declares, which merge with the module's own.
#[derive(Default)]
struct AugmentedNames(Vec<String>);

impl AugmentedNames {
    fn add(&mut self, name: &str) {
        if !self.0.iter().any(|n| n == name) {
            self.0.push(name.to_string());
        }
    }
}

impl<'a> Visit<'a> for AugmentedNames {
    fn visit_ts_type_reference(&mut self, it: &TSTypeReference<'a>) {
        if let TSTypeName::IdentifierReference(id) = &it.type_name {
            self.add(&id.name);
        }
        walk::walk_ts_type_reference(self, it);
    }

    fn visit_ts_interface_declaration(&mut self, it: &TSInterfaceDeclaration<'a>) {
        self.add(&it.id.name);
        walk::walk_ts_interface_declaration(self, it);
    }

    fn visit_ts_interface_heritage(&mut self, it: &TSInterfaceHeritage<'a>) {
        if let Expression::Identifier(id) = &it.expression {
            self.add(&id.name);
        }
        walk::walk_ts_interface_heritage(self, it);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxc_allocator::Allocator;
    use oxc_parser::Parser;
    use oxc_span::SourceType;

    /// Each use as `request:name`, `request:*` or `request:loader`, and each
    /// `export *` as `*request`.
    fn uses(src: &str) -> Vec<String> {
        let allocator = Allocator::default();
        let ret = Parser::new(&allocator, src, SourceType::tsx()).parse();
        assert!(ret.errors.is_empty(), "{:?}", ret.errors);
        let uses = collect(&ret.program);
        let mut out: Vec<String> = uses
            .usages
            .into_iter()
            .map(|u| match u.used {
                Used::Name(n) => format!("{}:{n}", u.request),
                Used::All => format!("{}:*", u.request),
                Used::Loader => format!("{}:loader", u.request),
                Used::Member { name, member } => {
                    format!("{}:{name}.{}", u.request, member.join("."))
                }
                Used::Whole(n) => format!("{}:{n}!", u.request),
            })
            .collect();
        out.extend(uses.reexport_all.into_iter().map(|r| format!("*{r}")));
        out
    }

    #[test]
    fn static_imports() {
        let src = "import d, { a, b as c } from './m';
import type { T } from './t';
import { type U } from './u';
import './side-effect';
import x = require('./cjs');
export * from './all';
export type * from './types';";
        assert_eq!(
            uses(src),
            [
                "./cjs:default",
                "./m:a",
                "./m:b",
                "./m:default",
                "./t:T",
                "./u:U",
                "*./all",
                "*./types"
            ]
        );
    }

    #[test]
    fn namespace_members() {
        let src = "import * as ns from './ns';
import * as types from './types';
import * as ui from './ui';
ns.a(); ns['b']; ns.c.d;
const { e, f: g } = ns;
let t: types.T;
const el = <ui.Button />;";
        assert_eq!(
            uses(src),
            ["./ns:a", "./ns:b", "./ns:c.d", "./ns:e", "./ns:f", "./types:T", "./ui:Button"]
        );
    }

    #[test]
    fn members_of_named_imports() {
        let src = "import { Color, ns, Other } from './m';
Color.Red; ns.a(); const { b } = ns;
Other.X; f(Other);";
        // `Other` is also passed to a call, so it is used by name too.
        assert_eq!(
            uses(src),
            ["./m:Color.Red", "./m:Other.X", "./m:Other", "./m:ns.a", "./m:ns.b"]
        );
    }

    #[test]
    fn namespaces_used_as_a_whole() {
        for src in [
            "import * as ns from './ns'; f(ns);",
            "import * as ns from './ns'; Object.keys(ns);",
            "import * as ns from './ns'; ns[key];",
            "import * as ns from './ns'; const { a, ...rest } = ns;",
            "import * as ns from './ns'; type N = typeof ns;",
        ] {
            assert_eq!(uses(src), ["./ns:*"], "{src}");
        }
        assert_eq!(uses("import * as ns from './ns';"), Vec::<String>::new());
    }

    #[test]
    fn module_augmentations() {
        let src = "declare module './events' {
  interface Events extends Base { created: Payload & { at: number } }
}";
        assert_eq!(uses(src), ["./events:Events", "./events:Base", "./events:Payload"]);
        // Imports referred to inside are used too.
        let src = "import * as t from './types';
declare module './events' { interface Events { created: t.Payload } }";
        assert_eq!(uses(src), ["./events:Events", "./types:Payload"]);
    }

    /// Each use as above, with `request:name!` for an export used as a
    /// whole.
    #[test]
    fn member_paths_and_whole_reads() {
        let src = "import { E, N, T, A } from './m';
import * as ns from './ns';
E.a; E['b']; N.x.y; ns.N.z.w; let t: T.Inner.Deep;
E[code]; A[3]; A['2000']; A['0x10'];";
        assert_eq!(
            uses(src),
            [
                "./m:A!",
                "./m:A.0x10",
                "./m:E!",
                "./m:E.a",
                "./m:E.b",
                "./m:N.x.y",
                "./m:T.Inner.Deep",
                "./ns:N.z.w"
            ]
        );
        for src in [
            "import { E } from './m'; Object.keys(E);",
            "import { E } from './m'; Object.values(E);",
            "import { E } from './m'; const e = E;",
            "import { E } from './m'; let e: E;",
            "import { E } from './m'; type K = keyof typeof E;",
            "import * as ns from './m'; Object.entries(ns.E);",
            "import * as ns from './m'; ns.E[key];",
        ] {
            assert_eq!(uses(src), ["./m:E!"], "{src}");
        }
        // Passing `E` around uses it but none of its members, as in knip.
        assert_eq!(uses("import { E } from './m'; f(E);"), ["./m:E"]);
        // A namespace import enumerated is used as a whole.
        assert_eq!(uses("import * as ns from './m'; Object.keys(ns);"), ["./m:*"]);
    }

    #[test]
    fn require_bindings() {
        let src = "const lib = require('./lib'); lib.x;
const { a, ...rest } = require('./rest');";
        assert_eq!(uses(src), ["./lib:default", "./rest:*", "./lib:x"]);
    }

    /// The same name bound to two modules in different scopes.
    #[test]
    fn rebound_names_use_both_modules() {
        let src = "async function one() { const mod = await import('./a'); mod.runA(); }
async function two() { const mod = await import('./b'); mod.runB(); }";
        assert_eq!(
            uses(src),
            ["./a:default", "./b:default", "./a:*", "./b:*", "./a:runA", "./a:runB"]
        );
    }

    #[test]
    fn imports_passed_on() {
        // Used through the re-export, not here...
        assert_eq!(uses("import { a } from './a'; export { a };"), Vec::<String>::new());
        assert_eq!(uses("import a from './a'; export default a;"), Vec::<String>::new());
        assert_eq!(uses("import * as ns from './ns'; export { ns };"), Vec::<String>::new());
        // ...unless also used here.
        assert_eq!(uses("import { a } from './a'; a(); export { a };"), ["./a:a"]);
        // `b` may be read like `a`, so every member of `a` is used.
        assert_eq!(uses("import { a } from './a'; export const b = a;"), ["./a:a!"]);
    }

    #[test]
    fn require_and_dynamic_imports() {
        let src = "const { a, b: c } = require('./r');
const whole = require('./whole');
require('./bare').x;
async function f() {
  const { d } = await import('./d');
  const m = await import('./m');
  m.e;
  import('./then').then(x => x);
  const lazy = React.lazy(() => import('./page'));
  route(async () => await import('./route'));
}
fork(require.resolve('./worker.js'));
new Worker(import.meta.resolve('./esm-worker.js'));";
        assert_eq!(
            uses(src),
            [
                "./r:a",
                "./r:b",
                "./whole:default",
                "./bare:default",
                "./d:d",
                "./m:default",
                "./then:*",
                "./page:loader",
                "./route:loader",
                "./worker.js:*",
                "./esm-worker.js:*",
                "./m:e"
            ]
        );
    }
}
