//! The exports of a module, read from its top-level statements.
//!
//! Each [`Export`] records the name it is exported under, the local binding
//! behind it, what kind of symbol it is, where it is, and the tags of the
//! comment on its statement (`@public`, `@internal`, `@alias`, ...). Checks
//! for unused and duplicate exports build on this.

use std::collections::{HashMap, HashSet};

use oxc_ast::ast::{
    Argument, ArrowFunctionExpression, BindingPatternKind, CallExpression, Declaration,
    ExportDefaultDeclarationKind, Expression, Function, FunctionBody, ImportDeclarationSpecifier,
    Program, Statement, TSAsExpression, TSModuleDeclarationName, TSModuleReference,
    TSSatisfiesExpression, TSType, TSTypeAssertion, TSTypeName, TSTypeQuery, TSTypeQueryExprName,
    TSTypeReference, VariableDeclarator,
};
use oxc_ast_visit::{Visit, walk};
use oxc_span::GetSpan;
use oxc_syntax::scope::ScopeFlags;

/// What kind of symbol an export is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ExportKind {
    /// A variable, an expression such as `export default 1 + 1`, or a name
    /// whose declaration isn't in the file (an imported binding, or the name
    /// in `export { a } from './a'`).
    Value,
    Function,
    Class,
    Enum,
    /// `export type A = ...`, or a name exported with `export type { A }`.
    Type,
    Interface,
    /// `export namespace N {}`, `export module N {}` or `export * as N`.
    Namespace,
}

impl ExportKind {
    /// Lower-case name, as in JSON output: `value`, `function`, `class`,
    /// `enum`, `type`, `interface` or `namespace`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Value => "value",
            Self::Function => "function",
            Self::Class => "class",
            Self::Enum => "enum",
            Self::Type => "type",
            Self::Interface => "interface",
            Self::Namespace => "namespace",
        }
    }
}

/// How a name came to be exported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ExportForm {
    /// `export const a = 1`, `export function f() {}`, `export enum E {}`.
    Declaration,
    /// `export { a }` or `export { a as b }` of a binding in the file,
    /// declared or imported.
    Specifier,
    /// `export default ...` or `export = ...`.
    Default,
    /// Forwards another module's export: `export { a } from './x'` or
    /// `export * as ns from './x'`.
    ReExport,
}

/// One name a module exports.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Export {
    /// The name importers use; `default` for the default export.
    pub name: String,
    /// The binding behind the export: the declared name, the local name in
    /// `export { local as name }`, or the identifier in `export default a`.
    /// For `export { a as b } from './x'` it is `a`, a name in `./x`. `None`
    /// for an anonymous default export or `export * as ns`.
    pub local: Option<String>,
    pub kind: ExportKind,
    pub form: ExportForm,
    /// For `export const b = a` and `export default a`: `a`, the binding it
    /// repeats under another name. Set only when `a` is declared or imported
    /// at the top level of the file.
    pub alias_of: Option<String>,
    /// Set when the export passes on another module's export: a re-export
    /// (`export { a } from './x'`, `export * as ns from './x'`), or an
    /// imported binding exported unchanged (`export { a }`,
    /// `export default a` or `export const b = a` where `a` is imported).
    pub forwarded: Option<Forwarded>,
    /// Whether the export is marked type-only: `export type { A }`,
    /// `export { type A }` or `export type * as ns from './x'`.
    pub type_only: bool,
    /// Local names the export's signature refers to in type positions
    /// (`T` in `function f(): T` or `const c: T`, and `x` in `typeof x`).
    /// Set for exported functions, classes and variables; knip counts a
    /// declaration as used when an export that refers to it is used.
    pub signature_refs: Vec<String>,
    /// 1-based line of the exported name.
    pub line: u32,
    /// Tags of the block comment right before the export's statement, without
    /// the `@`, e.g. `public` or `alias`.
    pub tags: Vec<String>,
}

/// Another module's export that an [`Export`] passes on.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Forwarded {
    /// The module, as written in the import or re-export.
    pub from: String,
    /// The name exported by `from`; `default` for its default export.
    /// `None` for the whole module, as in `export * as ns from './x'` or
    /// `import * as ns from './x'; export { ns }`.
    pub name: Option<String>,
}

impl Export {
    /// Whether the comment on the export has `@tag`.
    #[must_use]
    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags.iter().any(|t| t == tag)
    }

    /// Whether knip reports this export as an unused *type* rather than an
    /// unused export: a type alias, interface or enum declared in the
    /// export, or a name exported with `type`. Like knip, `export { A }` of
    /// an interface `A` counts as a value export.
    #[must_use]
    pub fn is_type(&self) -> bool {
        match self.form {
            ExportForm::Declaration => {
                matches!(self.kind, ExportKind::Type | ExportKind::Interface | ExportKind::Enum)
            }
            ExportForm::Default => matches!(self.kind, ExportKind::Interface | ExportKind::Enum),
            ExportForm::Specifier | ExportForm::ReExport => self.type_only,
        }
    }
}

/// A name bound at the top level of a module.
#[derive(Debug, Clone, Copy)]
struct Binding<'a> {
    kind: ExportKind,
    /// For an imported binding: the module, and the name imported from it
    /// (`None` for a namespace import).
    import: Option<(&'a str, Option<&'a str>)>,
}

impl Binding<'_> {
    fn forwarded(&self) -> Option<Forwarded> {
        self.import.map(|(from, name)| Forwarded {
            from: from.to_string(),
            name: name.map(str::to_string),
        })
    }
}

/// Lists the exports of `program`. `line` maps a byte offset to its 1-based
/// line.
pub(crate) fn collect(program: &Program<'_>, line: &dyn Fn(u32) -> u32) -> Vec<Export> {
    let mut bindings = HashMap::new();
    for stmt in &program.body {
        top_level_bindings(stmt, &mut bindings);
    }
    let binding = |name: &str| bindings.get(name).copied();
    // `export const b = a` or `export default a` repeats `a` when `a` is a
    // top-level binding.
    let alias = |expr: Option<&Expression<'_>>| match expr {
        Some(Expression::Identifier(id)) => binding(&id.name).map(|b| (id.name.to_string(), b)),
        _ => None,
    };

    let mut out: Vec<Export> = Vec::new();
    // Calls in each export's signature, by index in `out`.
    let mut calls: Vec<(usize, Vec<Call>)> = Vec::new();
    // Index in `out` of each exported function, so overloads merge into one.
    let mut functions: HashMap<String, usize> = HashMap::new();
    for stmt in &program.body {
        if !matches!(
            stmt,
            Statement::ExportNamedDeclaration(_)
                | Statement::ExportDefaultDeclaration(_)
                | Statement::ExportAllDeclaration(_)
                | Statement::TSExportAssignment(_)
        ) {
            continue;
        }
        let tags = comment_tags(program, stmt.span().start);
        let new = |name: &str, local: Option<&str>, kind, form, at: u32| Export {
            name: name.to_string(),
            local: local.map(str::to_string),
            kind,
            form,
            alias_of: None,
            forwarded: None,
            type_only: false,
            signature_refs: Vec::new(),
            line: line(at),
            tags: tags.clone(),
        };

        match stmt {
            Statement::ExportNamedDeclaration(decl) => {
                if let Some(declaration) = &decl.declaration {
                    for_each_declared(declaration, true, &mut |name, kind, at, init, refs| {
                        if kind == ExportKind::Function {
                            // Overload signatures and the implementation are
                            // one export, whose signature is all of them.
                            if let Some(&i) = functions.get(name) {
                                out[i].tags.extend(tags.iter().cloned());
                                for r in refs.names {
                                    if !out[i].signature_refs.contains(&r) {
                                        out[i].signature_refs.push(r);
                                    }
                                }
                                if !refs.calls.is_empty() {
                                    calls.push((i, refs.calls));
                                }
                                return;
                            }
                            functions.insert(name.to_string(), out.len());
                        }
                        let mut e = new(name, Some(name), kind, ExportForm::Declaration, at);
                        if let Some((a, b)) = alias(init).filter(|(a, _)| a != name) {
                            e.forwarded = b.forwarded();
                            e.alias_of = Some(a);
                        }
                        e.signature_refs = refs.names;
                        if !refs.calls.is_empty() {
                            calls.push((out.len(), refs.calls));
                        }
                        out.push(e);
                    });
                }
                for spec in &decl.specifiers {
                    let local = spec.local.name();
                    let is_type = decl.export_kind.is_type() || spec.export_kind.is_type();
                    let (form, bound) = if decl.source.is_some() {
                        (ExportForm::ReExport, None)
                    } else {
                        (ExportForm::Specifier, binding(&local))
                    };
                    let forwarded = match &decl.source {
                        Some(source) => Some(Forwarded {
                            from: source.value.to_string(),
                            name: Some(local.to_string()),
                        }),
                        None => bound.and_then(|b| b.forwarded()),
                    };
                    let kind = match bound {
                        _ if is_type => ExportKind::Type,
                        Some(b) => b.kind,
                        None => ExportKind::Value,
                    };
                    let at = spec.exported.span().start;
                    let mut e = new(&spec.exported.name(), Some(&local), kind, form, at);
                    e.forwarded = forwarded;
                    e.type_only = is_type;
                    out.push(e);
                }
            }
            Statement::ExportDefaultDeclaration(decl) => {
                let at = decl.declaration.span().start;
                match &decl.declaration {
                    ExportDefaultDeclarationKind::FunctionDeclaration(f) => {
                        let local = f.id.as_ref().map(|id| id.name.as_str());
                        out.push(new(
                            "default",
                            local,
                            ExportKind::Function,
                            ExportForm::Default,
                            at,
                        ));
                    }
                    ExportDefaultDeclarationKind::ClassDeclaration(c) => {
                        let local = c.id.as_ref().map(|id| id.name.as_str());
                        out.push(new("default", local, ExportKind::Class, ExportForm::Default, at));
                    }
                    ExportDefaultDeclarationKind::TSInterfaceDeclaration(i) => {
                        let local = Some(i.id.name.as_str());
                        out.push(new(
                            "default",
                            local,
                            ExportKind::Interface,
                            ExportForm::Default,
                            at,
                        ));
                    }
                    other => {
                        let expr = other.as_expression();
                        let aliased = alias(expr);
                        let kind = aliased.as_ref().map_or(ExportKind::Value, |(_, b)| b.kind);
                        let local = aliased.as_ref().map(|(a, _)| a.as_str());
                        let mut e = new("default", local, kind, ExportForm::Default, at);
                        e.forwarded = aliased.as_ref().and_then(|(_, b)| b.forwarded());
                        e.alias_of = aliased.map(|(a, _)| a);
                        out.push(e);
                    }
                }
            }
            Statement::TSExportAssignment(assign) => {
                // `export = a` is the module's default export. knip doesn't
                // count it as repeating `a`, so `alias_of` stays unset.
                let aliased = alias(Some(&assign.expression));
                let kind = aliased.as_ref().map_or(ExportKind::Value, |(_, b)| b.kind);
                let local = aliased.as_ref().map(|(a, _)| a.as_str());
                let at = assign.expression.span().start;
                out.push(new("default", local, kind, ExportForm::Default, at));
            }
            Statement::ExportAllDeclaration(decl) => {
                if let Some(exported) = &decl.exported {
                    let kind = if decl.export_kind.is_type() {
                        ExportKind::Type
                    } else {
                        ExportKind::Namespace
                    };
                    let at = exported.span().start;
                    let mut e = new(&exported.name(), None, kind, ExportForm::ReExport, at);
                    e.forwarded =
                        Some(Forwarded { from: decl.source.value.to_string(), name: None });
                    e.type_only = decl.export_kind.is_type();
                    out.push(e);
                }
            }
            _ => {}
        }
    }
    if !calls.is_empty() {
        follow_calls(program, &mut out, calls);
    }
    out
}

/// A function or variable declared at the top level of a module.
#[derive(Clone, Copy)]
enum Local<'d, 'a> {
    Function(&'d Function<'a>),
    Variable(&'d VariableDeclarator<'a>),
}

/// Adds to each export's [`Export::signature_refs`] the type names that
/// the local functions its signature calls return, and the types of the
/// locals it passes to calls, as knip does. So in
/// `export const read = () => helper()`, the return type of `helper` counts.
fn follow_calls(program: &Program<'_>, out: &mut [Export], calls: Vec<(usize, Vec<Call>)>) {
    let mut locals: HashMap<&str, Local<'_, '_>> = HashMap::new();
    for stmt in &program.body {
        let declaration = match stmt {
            Statement::ExportNamedDeclaration(decl) => decl.declaration.as_ref(),
            Statement::ExportDefaultDeclaration(decl) => {
                if let ExportDefaultDeclarationKind::FunctionDeclaration(f) = &decl.declaration
                    && let Some(id) = &f.id
                {
                    locals.entry(id.name.as_str()).or_insert(Local::Function(f));
                }
                None
            }
            other => other.as_declaration(),
        };
        match declaration {
            Some(Declaration::FunctionDeclaration(f)) => {
                if let Some(id) = &f.id {
                    locals.entry(id.name.as_str()).or_insert(Local::Function(f));
                }
            }
            Some(Declaration::VariableDeclaration(var)) => {
                for d in &var.declarations {
                    if let BindingPatternKind::BindingIdentifier(id) = &d.id.kind {
                        locals.entry(id.name.as_str()).or_insert(Local::Variable(d));
                    }
                }
            }
            _ => {}
        }
    }
    for (i, pending) in calls {
        let mut seen: HashSet<Call> = HashSet::new();
        let mut queue = pending;
        while let Some(call) = queue.pop() {
            let Some(&local) = locals.get(call.name.as_str()) else { continue };
            if !seen.insert(call.clone()) {
                continue;
            }
            let refs = if call.argument { local_type(local) } else { call_result(local) };
            for name in refs.names {
                if !out[i].signature_refs.contains(&name) {
                    out[i].signature_refs.push(name);
                }
            }
            queue.extend(refs.calls);
        }
    }
}

/// Whether `f` is `(p: T) => p.member`: its result is the member's type,
/// not `T`, so knip leaves `T` out.
fn returns_param_member(f: &ArrowFunctionExpression<'_>) -> bool {
    let Some(Statement::ExpressionStatement(stmt)) = f.body.statements.first() else {
        return false;
    };
    let Expression::StaticMemberExpression(member) = &stmt.expression else { return false };
    let Expression::Identifier(object) = &member.object else { return false };
    f.expression
        && f.params.items.iter().any(|p| {
            matches!(&p.pattern.kind, BindingPatternKind::BindingIdentifier(id) if id.name == object.name)
                && matches!(
                    p.pattern.type_annotation.as_ref().map(|a| &a.type_annotation),
                    Some(TSType::TSTypeReference(r)) if r.type_arguments.is_none()
                )
        })
}

/// The type names in a local's signature, function bodies left out.
fn local_type(local: Local<'_, '_>) -> TypeRefs {
    let mut v = TypeRefs::signature(true);
    match local {
        Local::Function(f) => v.visit_function(f, ScopeFlags::empty()),
        Local::Variable(d) => v.visit_variable_declarator(d),
    }
    v
}

/// The type names in what calling a local returns: its declared return type
/// when it has one, or else its whole signature.
fn call_result(local: Local<'_, '_>) -> TypeRefs {
    let mut v = TypeRefs::signature(true);
    let function = match local {
        Local::Variable(d) => match d.id.type_annotation.as_ref().map(|a| &a.type_annotation) {
            Some(TSType::TSFunctionType(f)) => {
                if let Some(params) = &f.type_parameters {
                    v.visit_ts_type_parameter_declaration(params);
                }
                v.visit_ts_type_annotation(&f.return_type);
                return v;
            }
            Some(other) => {
                v.visit_ts_type(other);
                return v;
            }
            None => match &d.init {
                Some(Expression::ArrowFunctionExpression(f)) => {
                    if let Some(ret) = &f.return_type {
                        if let Some(params) = &f.type_parameters {
                            v.visit_ts_type_parameter_declaration(params);
                        }
                        v.visit_ts_type_annotation(ret);
                    } else if !returns_param_member(f) {
                        v.visit_arrow_function_expression(f);
                    }
                    return v;
                }
                Some(Expression::FunctionExpression(f)) => f,
                Some(other) => {
                    v.visit_expression(other);
                    return v;
                }
                None => return v,
            },
        },
        Local::Function(f) => f,
    };
    match &function.return_type {
        Some(ret) => {
            if let Some(params) = &function.type_parameters {
                v.visit_ts_type_parameter_declaration(params);
            }
            v.visit_ts_type_annotation(ret);
        }
        None => v.visit_function(function, ScopeFlags::empty()),
    }
    v
}

/// Calls `f(name, kind, offset, initializer, refs)` for each binding
/// `declaration` declares. The initializer is given only for a plain
/// `name = init`. With `with_refs`, `refs` are the binding's
/// [`Export::signature_refs`]; otherwise it is empty.
fn for_each_declared<'d, 'a: 'd>(
    declaration: &'d Declaration<'a>,
    with_refs: bool,
    f: &mut dyn FnMut(&'d str, ExportKind, u32, Option<&'d Expression<'a>>, TypeRefs),
) {
    let refs = |collect: &dyn Fn(&mut TypeRefs)| {
        let mut v = TypeRefs::default();
        if with_refs {
            collect(&mut v);
        }
        v
    };
    match declaration {
        Declaration::VariableDeclaration(var) => {
            for d in &var.declarations {
                // `export const b = a` repeats `a`; destructuring does not.
                let init = if d.id.kind.is_binding_identifier() { d.init.as_ref() } else { None };
                let refs = refs(&|v| match (&d.id.type_annotation, &d.init) {
                    (Some(annotation), _) => v.visit_ts_type_annotation(annotation),
                    (None, Some(init)) => {
                        v.signature_only = has_return_type(init);
                        v.visit_expression(init);
                    }
                    (None, None) => {}
                });
                for id in d.id.get_binding_identifiers() {
                    f(id.name.as_str(), ExportKind::Value, id.span.start, init, refs.clone());
                }
            }
        }
        Declaration::FunctionDeclaration(func) => {
            if let Some(id) = &func.id {
                let refs = refs(&|v| {
                    v.signature_only = func.return_type.is_some();
                    v.visit_function(func, ScopeFlags::empty());
                });
                f(id.name.as_str(), ExportKind::Function, id.span.start, None, refs);
            }
        }
        Declaration::ClassDeclaration(class) => {
            if let Some(id) = &class.id {
                let refs = refs(&|v| {
                    v.signature_only = true;
                    v.visit_class(class);
                });
                f(id.name.as_str(), ExportKind::Class, id.span.start, None, refs);
            }
        }
        Declaration::TSTypeAliasDeclaration(t) => {
            f(t.id.name.as_str(), ExportKind::Type, t.id.span.start, None, TypeRefs::default());
        }
        Declaration::TSInterfaceDeclaration(i) => {
            let none = TypeRefs::default();
            f(i.id.name.as_str(), ExportKind::Interface, i.id.span.start, None, none);
        }
        Declaration::TSEnumDeclaration(e) => {
            f(e.id.name.as_str(), ExportKind::Enum, e.id.span.start, None, TypeRefs::default());
        }
        Declaration::TSModuleDeclaration(m) => {
            if let TSModuleDeclarationName::Identifier(id) = &m.id {
                let none = TypeRefs::default();
                f(id.name.as_str(), ExportKind::Namespace, id.span.start, None, none);
            }
        }
        Declaration::TSImportEqualsDeclaration(i) => {
            f(i.id.name.as_str(), ExportKind::Value, i.id.span.start, None, TypeRefs::default());
        }
    }
}

/// Whether `expr` is a function with an explicit return type, so its body
/// doesn't shape its type.
fn has_return_type(expr: &Expression<'_>) -> bool {
    match expr {
        Expression::ArrowFunctionExpression(f) => f.return_type.is_some(),
        Expression::FunctionExpression(f) => f.return_type.is_some(),
        _ => false,
    }
}

/// Collects the names referred to in type positions, as knip's
/// `collectRefsInType` does: type references `T` and type queries
/// `typeof x`. With `signature_only`, function bodies are skipped. Inside a
/// body, variable annotations and type assertions are skipped, since they
/// rarely shape the type of the export.
///
/// It also notes the functions called (`helper()`) and, outside function
/// bodies, the identifiers passed to calls (`wrap(inner)`), whose own types
/// may shape the export's; [`follow_calls`] adds those.
#[derive(Debug, Default, Clone)]
struct TypeRefs {
    names: Vec<String>,
    calls: Vec<Call>,
    signature_only: bool,
    body_depth: u32,
}

/// A local name a signature calls, or passes to a call.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Call {
    name: String,
    argument: bool,
}

impl TypeRefs {
    fn add(&mut self, name: &str) {
        if !self.names.iter().any(|n| n == name) {
            self.names.push(name.to_string());
        }
    }

    fn signature(signature_only: bool) -> Self {
        Self { signature_only, ..Self::default() }
    }
}

impl<'a> Visit<'a> for TypeRefs {
    fn visit_ts_type_reference(&mut self, it: &TSTypeReference<'a>) {
        if let TSTypeName::IdentifierReference(id) = &it.type_name {
            self.add(&id.name);
        }
        walk::walk_ts_type_reference(self, it);
    }

    fn visit_ts_type_query(&mut self, it: &TSTypeQuery<'a>) {
        if let TSTypeQueryExprName::IdentifierReference(id) = &it.expr_name {
            self.add(&id.name);
        }
    }

    fn visit_call_expression(&mut self, it: &CallExpression<'a>) {
        if let Expression::Identifier(callee) = &it.callee {
            self.calls.push(Call { name: callee.name.to_string(), argument: false });
        }
        if self.body_depth == 0 {
            for arg in &it.arguments {
                if let Argument::Identifier(id) = arg {
                    self.calls.push(Call { name: id.name.to_string(), argument: true });
                }
            }
        }
        walk::walk_call_expression(self, it);
    }

    fn visit_function_body(&mut self, it: &FunctionBody<'a>) {
        if self.signature_only {
            return;
        }
        self.body_depth += 1;
        walk::walk_function_body(self, it);
        self.body_depth -= 1;
    }

    fn visit_variable_declarator(&mut self, it: &VariableDeclarator<'a>) {
        if self.body_depth == 0 {
            walk::walk_variable_declarator(self, it);
        } else if let Some(init) = &it.init {
            self.visit_expression(init);
        }
    }

    fn visit_ts_as_expression(&mut self, it: &TSAsExpression<'a>) {
        if self.body_depth == 0 {
            walk::walk_ts_as_expression(self, it);
        } else {
            self.visit_expression(&it.expression);
        }
    }

    fn visit_ts_satisfies_expression(&mut self, it: &TSSatisfiesExpression<'a>) {
        if self.body_depth == 0 {
            walk::walk_ts_satisfies_expression(self, it);
        } else {
            self.visit_expression(&it.expression);
        }
    }

    fn visit_ts_type_assertion(&mut self, it: &TSTypeAssertion<'a>) {
        if self.body_depth == 0 {
            walk::walk_ts_type_assertion(self, it);
        } else {
            self.visit_expression(&it.expression);
        }
    }
}

/// Adds the names `stmt` binds at the top level of the module, by a
/// declaration or an import, to `bindings`. The first binding of a name wins,
/// so a type and a value of the same name keep the first one's kind.
fn top_level_bindings<'a>(stmt: &'a Statement<'_>, bindings: &mut HashMap<&'a str, Binding<'a>>) {
    let mut add = |name: &'a str, kind, import| {
        bindings.entry(name).or_insert(Binding { kind, import });
    };
    let declaration = match stmt {
        Statement::ImportDeclaration(decl) => {
            let from = decl.source.value.as_str();
            for spec in decl.specifiers.iter().flatten() {
                let imported = match spec {
                    ImportDeclarationSpecifier::ImportSpecifier(s) => {
                        Some(s.imported.name().as_str())
                    }
                    ImportDeclarationSpecifier::ImportDefaultSpecifier(_) => Some("default"),
                    ImportDeclarationSpecifier::ImportNamespaceSpecifier(_) => None,
                };
                add(spec.local().name.as_str(), ExportKind::Value, Some((from, imported)));
            }
            return;
        }
        Statement::TSImportEqualsDeclaration(decl) => {
            // `import a = require('m')` imports `m`'s default export, as knip
            // reads it; `import a = N.B` aliases a namespace member.
            let import = match &decl.module_reference {
                TSModuleReference::ExternalModuleReference(ext) => {
                    Some((ext.expression.value.as_str(), Some("default")))
                }
                _ => None,
            };
            add(decl.id.name.as_str(), ExportKind::Value, import);
            return;
        }
        Statement::ExportNamedDeclaration(decl) => match &decl.declaration {
            Some(d) => d,
            None => return,
        },
        Statement::ExportDefaultDeclaration(decl) => {
            match &decl.declaration {
                ExportDefaultDeclarationKind::FunctionDeclaration(f) => {
                    if let Some(id) = &f.id {
                        add(id.name.as_str(), ExportKind::Function, None);
                    }
                }
                ExportDefaultDeclarationKind::ClassDeclaration(c) => {
                    if let Some(id) = &c.id {
                        add(id.name.as_str(), ExportKind::Class, None);
                    }
                }
                _ => {}
            }
            return;
        }
        other => match other.as_declaration() {
            Some(d) => d,
            None => return,
        },
    };
    for_each_declared(declaration, false, &mut |name, kind, _, _, _| add(name, kind, None));
}

/// Tags of the block comment right before `start`, as knip reads them: the
/// nearest `/* ... */` or `/** ... */` comment, separated from `start` only
/// by whitespace and `//` comments.
fn comment_tags(program: &Program<'_>, start: u32) -> Vec<String> {
    let source = program.source_text;
    let mut cursor = start as usize;
    let end = program.comments.partition_point(|c| c.span.end <= start);
    for comment in program.comments[..end].iter().rev() {
        let gap = source.get(comment.span.end as usize..cursor).unwrap_or("x");
        if !gap.trim().is_empty() {
            break;
        }
        if comment.is_block() {
            return parse_tags(comment.content_span().source_text(source)).collect();
        }
        cursor = comment.span.start as usize;
    }
    Vec::new()
}

/// The block tags in a comment's text: each `@name` at the start of the
/// comment or of a line, or after whitespace. Inline tags such as
/// `{@link x}` and addresses such as `a@b.c` are not block tags, which is
/// stricter than knip. A name is letters, digits and `_`, as in knip.
fn parse_tags(text: &str) -> impl Iterator<Item = String> + '_ {
    text.match_indices('@').filter_map(move |(at, _)| {
        let before = text[..at].chars().next_back();
        if !before.is_none_or(|c| c.is_whitespace() || c == '*') {
            return None;
        }
        let name: String =
            text[at + 1..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
        (!name.is_empty()).then_some(name)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxc_allocator::Allocator;
    use oxc_parser::Parser;
    use oxc_span::SourceType;

    fn exports(src: &str) -> Vec<Export> {
        let allocator = Allocator::default();
        let ret = Parser::new(&allocator, src, SourceType::ts()).parse();
        assert!(ret.errors.is_empty(), "{:?}", ret.errors);
        let starts: Vec<u32> = std::iter::once(0)
            .chain(src.match_indices('\n').map(|(i, _)| u32::try_from(i + 1).unwrap()))
            .collect();
        let line = |at: u32| u32::try_from(starts.partition_point(|&s| s <= at)).unwrap();
        collect(&ret.program, &line)
    }

    /// `name<-local` (or `name`), the form, and `=alias` when set.
    fn summary(src: &str) -> Vec<String> {
        exports(src)
            .iter()
            .map(|e| {
                let mut s = e.name.clone();
                if let Some(local) = e.local.as_ref().filter(|l| **l != e.name) {
                    s = format!("{s}<-{local}");
                }
                s = format!("{s} {:?}", e.form);
                if let Some(a) = &e.alias_of {
                    s = format!("{s} ={a}");
                }
                s
            })
            .collect()
    }

    #[test]
    fn declarations_specifiers_and_defaults() {
        let src = "import { imp } from './x';
const a = 1, b = 2;
export const c = a, { d, e: f } = obj;
export function g() {}
export { a, b as h, imp, imp as imp2 };
export { x as y } from './x';
export * as ns from './ns';
export * from './all';
export default a;";
        assert_eq!(
            summary(src),
            [
                "c Declaration =a",
                "d Declaration",
                "f Declaration",
                "g Declaration",
                "a Specifier",
                "h<-b Specifier",
                "imp Specifier",
                "imp2<-imp Specifier",
                "y<-x ReExport",
                "ns ReExport",
                "default<-a Default =a",
            ]
        );
    }

    #[test]
    fn alias_of_is_only_a_top_level_binding() {
        let src = "import { imp } from './x';
export const a = undefined;
export const b = imp;
export const c = window;
export const d = d2;
function d2() {}
export default imp;";
        assert_eq!(
            summary(src),
            [
                "a Declaration",
                "b Declaration =imp",
                "c Declaration",
                "d Declaration =d2",
                "default<-imp Default =imp",
            ]
        );
    }

    #[test]
    fn typescript_kinds() {
        let src = "export type T = 1;
export interface I {}
export enum E { A }
export namespace N { export const inner = 1; }
export declare module M {}
declare module 'ambient' { export const x: 1; }
export type { T as U };
export default interface D {}";
        let kinds: Vec<(String, ExportKind)> =
            exports(src).into_iter().map(|e| (e.name, e.kind)).collect();
        assert_eq!(
            kinds,
            [
                ("T".into(), ExportKind::Type),
                ("I".into(), ExportKind::Interface),
                ("E".into(), ExportKind::Enum),
                ("N".into(), ExportKind::Namespace),
                ("M".into(), ExportKind::Namespace),
                ("U".into(), ExportKind::Type),
                ("default".into(), ExportKind::Interface),
            ]
        );
    }

    #[test]
    fn lines_and_comment_tags() {
        // As in knip: the nearest block comment counts, JSDoc or not, across
        // whitespace and `//` lines, even on the previous statement's line.
        let src = "/** @module x */
/** Docs. @public */
export const a = 1;
/**
 * @internal see {@link a}, mail me@example.com
 * @alias-of
 */
// eslint-disable-next-line
export default a;
/* @public, not JSDoc */
export function f() {}
export const g = 1; /** @beta */ export const h = g;
/** @public */
const i = 1;
export { i };";
        let got: Vec<(String, u32, Vec<String>)> =
            exports(src).into_iter().map(|e| (e.name, e.line, e.tags)).collect();
        let tags = |t: &[&str]| t.iter().map(|t| (*t).to_string()).collect::<Vec<_>>();
        assert_eq!(
            got,
            [
                ("a".into(), 3, tags(&["public"])),
                ("default".into(), 9, tags(&["internal", "alias"])),
                ("f".into(), 11, tags(&["public"])),
                ("g".into(), 12, tags(&[])),
                ("h".into(), 12, tags(&["beta"])),
                ("i".into(), 15, tags(&[])),
            ]
        );
    }

    #[test]
    fn overloads_merge_and_export_assignment() {
        let src = "/** @public */
export function f(): void;
export function f(x?: number) {}
function g() {}
export default g;";
        let got: Vec<(String, ExportKind, u32, Vec<String>)> =
            exports(src).into_iter().map(|e| (e.name, e.kind, e.line, e.tags)).collect();
        assert_eq!(
            got,
            [
                ("f".into(), ExportKind::Function, 2, vec!["public".to_string()]),
                ("default".into(), ExportKind::Function, 5, vec![]),
            ]
        );
        let assign = exports("class C {}\nexport = C;");
        assert_eq!(
            (assign[0].name.as_str(), assign[0].kind, assign[0].form, &assign[0].alias_of),
            ("default", ExportKind::Class, ExportForm::Default, &None)
        );
    }

    #[test]
    fn forwarded_imports() {
        let src = "import { x as x2 } from './x';
import * as ns from './ns';
import d from './d';
export { x2, ns as space };
export const y = x2;
const z = 1;
export { z };
export { a as b } from './a';
export * as all from './all';
export default d;";
        // `name <- module:imported`, or `module:*` for a whole namespace.
        let got: Vec<String> = exports(src)
            .into_iter()
            .map(|e| match e.forwarded {
                Some(f) => format!("{} <- {}:{}", e.name, f.from, f.name.as_deref().unwrap_or("*")),
                None => e.name,
            })
            .collect();
        assert_eq!(
            got,
            [
                "x2 <- ./x:x",
                "space <- ./ns:*",
                "y <- ./x:x",
                "z",
                "b <- ./a:a",
                "all <- ./all:*",
                "default <- ./d:default",
            ]
        );
    }

    #[test]
    fn types_as_knip_groups_them() {
        let src = "export type T = 1;
export interface I {}
export enum E { A }
export class C {}
interface J {}
export { J };
export type { J as K };
export { type J as L };
export type * as N from './n';
export default interface D {}";
        let got: Vec<(String, bool)> =
            exports(src).into_iter().map(|e| (e.name.clone(), e.is_type())).collect();
        let want: Vec<(String, bool)> = [
            ("T", true),
            ("I", true),
            ("E", true),
            ("C", false),
            ("J", false),
            ("K", true),
            ("L", true),
            ("N", true),
            ("default", true),
        ]
        .into_iter()
        .map(|(n, t)| (n.to_string(), t))
        .collect();
        assert_eq!(got, want);
    }

    #[test]
    fn signature_refs() {
        let src = "export function f(p: P): R { const x: Local = g as Cast; return x; }
export function h(p: P) { const x: Local = 1; return y as Cast; }
export const a: A = make();
export const b = (q: Q): B => { let z: Z; return z; };
export const c = new Map<K, V>();
export class K2 extends Base implements I { prop: Prop; m(): M { let n: N; return n; } }
export type T = U;
export function o(p: O1): R1;
export function o(p: O2): R2;
export function o(p: unknown) { return p; }";
        let got: Vec<(String, Vec<String>)> =
            exports(src).into_iter().map(|e| (e.name, e.signature_refs)).collect();
        let refs = |r: &[&str]| r.iter().map(|r| (*r).to_string()).collect::<Vec<_>>();
        assert_eq!(
            got,
            [
                ("f".into(), refs(&["P", "R"])),
                ("h".into(), refs(&["P"])),
                ("a".into(), refs(&["A"])),
                ("b".into(), refs(&["Q", "B"])),
                ("c".into(), refs(&["K", "V"])),
                ("K2".into(), refs(&["Prop", "M"])),
                ("T".into(), refs(&[])),
                // Every overload's signature counts.
                ("o".into(), refs(&["O1", "R1", "O2", "R2"])),
            ]
        );
    }
}
