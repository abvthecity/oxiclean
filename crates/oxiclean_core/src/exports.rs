//! The exports of a module, read from its top-level statements.
//!
//! Each [`Export`] records the name it is exported under, the local binding
//! behind it, what kind of symbol it is, where it is, and the tags of the
//! comment on its statement (`@public`, `@internal`, `@alias`, ...). Checks
//! for unused and duplicate exports build on this.

use std::collections::HashMap;

use oxc_ast::ast::{
    Declaration, ExportDefaultDeclarationKind, Expression, Program, Statement,
    TSModuleDeclarationName, TSModuleReference,
};
use oxc_span::GetSpan;

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
    /// When the export passes an imported binding on unchanged
    /// (`export { a }` or `export const b = a` where `a` is imported): the
    /// module it is imported from, as written. knip treats these as
    /// re-exports when that module is part of the project.
    pub forwarded_from: Option<String>,
    /// 1-based line of the exported name.
    pub line: u32,
    /// Tags of the block comment right before the export's statement, without
    /// the `@`, e.g. `public` or `alias`.
    pub tags: Vec<String>,
}

impl Export {
    /// Whether the comment on the export has `@tag`.
    #[must_use]
    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags.iter().any(|t| t == tag)
    }
}

/// A name bound at the top level of a module.
#[derive(Debug, Clone, Copy)]
struct Binding<'a> {
    kind: ExportKind,
    /// The module an imported binding comes from.
    source: Option<&'a str>,
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
            forwarded_from: None,
            line: line(at),
            tags: tags.clone(),
        };

        match stmt {
            Statement::ExportNamedDeclaration(decl) => {
                if let Some(declaration) = &decl.declaration {
                    for_each_declared(declaration, &mut |name, kind, at, init| {
                        if kind == ExportKind::Function {
                            // Overload signatures and the implementation are
                            // one export.
                            if let Some(&i) = functions.get(name) {
                                out[i].tags.extend(tags.iter().cloned());
                                return;
                            }
                            functions.insert(name.to_string(), out.len());
                        }
                        let mut e = new(name, Some(name), kind, ExportForm::Declaration, at);
                        if let Some((a, b)) = alias(init).filter(|(a, _)| a != name) {
                            e.forwarded_from = b.source.map(str::to_string);
                            e.alias_of = Some(a);
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
                    let kind = match bound {
                        _ if is_type => ExportKind::Type,
                        Some(b) => b.kind,
                        None => ExportKind::Value,
                    };
                    let at = spec.exported.span().start;
                    let mut e = new(&spec.exported.name(), Some(&local), kind, form, at);
                    e.forwarded_from = bound.and_then(|b| b.source).map(str::to_string);
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
                    out.push(new(&exported.name(), None, kind, ExportForm::ReExport, at));
                }
            }
            _ => {}
        }
    }
    out
}

/// Calls `f(name, kind, offset, initializer)` for each binding `declaration`
/// declares. The initializer is given only for a plain `name = init`.
fn for_each_declared<'d, 'a: 'd>(
    declaration: &'d Declaration<'a>,
    f: &mut dyn FnMut(&'d str, ExportKind, u32, Option<&'d Expression<'a>>),
) {
    match declaration {
        Declaration::VariableDeclaration(var) => {
            for d in &var.declarations {
                // `export const b = a` repeats `a`; destructuring does not.
                let init = if d.id.kind.is_binding_identifier() { d.init.as_ref() } else { None };
                for id in d.id.get_binding_identifiers() {
                    f(id.name.as_str(), ExportKind::Value, id.span.start, init);
                }
            }
        }
        Declaration::FunctionDeclaration(func) => {
            if let Some(id) = &func.id {
                f(id.name.as_str(), ExportKind::Function, id.span.start, None);
            }
        }
        Declaration::ClassDeclaration(class) => {
            if let Some(id) = &class.id {
                f(id.name.as_str(), ExportKind::Class, id.span.start, None);
            }
        }
        Declaration::TSTypeAliasDeclaration(t) => {
            f(t.id.name.as_str(), ExportKind::Type, t.id.span.start, None);
        }
        Declaration::TSInterfaceDeclaration(i) => {
            f(i.id.name.as_str(), ExportKind::Interface, i.id.span.start, None);
        }
        Declaration::TSEnumDeclaration(e) => {
            f(e.id.name.as_str(), ExportKind::Enum, e.id.span.start, None);
        }
        Declaration::TSModuleDeclaration(m) => {
            if let TSModuleDeclarationName::Identifier(id) = &m.id {
                f(id.name.as_str(), ExportKind::Namespace, id.span.start, None);
            }
        }
        Declaration::TSImportEqualsDeclaration(i) => {
            f(i.id.name.as_str(), ExportKind::Value, i.id.span.start, None);
        }
    }
}

/// Adds the names `stmt` binds at the top level of the module, by a
/// declaration or an import, to `bindings`. The first binding of a name wins,
/// so a type and a value of the same name keep the first one's kind.
fn top_level_bindings<'a>(stmt: &'a Statement<'_>, bindings: &mut HashMap<&'a str, Binding<'a>>) {
    let mut add = |name: &'a str, kind, source| {
        bindings.entry(name).or_insert(Binding { kind, source });
    };
    let declaration = match stmt {
        Statement::ImportDeclaration(decl) => {
            for spec in decl.specifiers.iter().flatten() {
                add(
                    spec.local().name.as_str(),
                    ExportKind::Value,
                    Some(decl.source.value.as_str()),
                );
            }
            return;
        }
        Statement::TSImportEqualsDeclaration(decl) => {
            // `import a = require('m')` imports `m`; `import a = N.B` aliases
            // a namespace member.
            let source = match &decl.module_reference {
                TSModuleReference::ExternalModuleReference(ext) => {
                    Some(ext.expression.value.as_str())
                }
                _ => None,
            };
            add(decl.id.name.as_str(), ExportKind::Value, source);
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
    for_each_declared(declaration, &mut |name, kind, _, _| add(name, kind, None));
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
        let src = "import { x } from './x';
import * as ns from './ns';
export { x, ns as space };
export const y = x;
const z = 1;
export { z };";
        let got: Vec<(String, Option<String>)> =
            exports(src).into_iter().map(|e| (e.name, e.forwarded_from)).collect();
        let from = |m: &str| Some(m.to_string());
        assert_eq!(
            got,
            [
                ("x".into(), from("./x")),
                ("space".into(), from("./ns")),
                ("y".into(), from("./x")),
                ("z".into(), None),
            ]
        );
    }
}
