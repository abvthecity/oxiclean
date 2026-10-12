//! The members of exported enums and namespaces, for knip's `enumMembers`
//! and `namespaceMembers`.
//!
//! An enum's members are its keys. A namespace's members are what it
//! exports, with nested namespaces flattened into dotted paths
//! (`Fruits.Tropical.mango` is the member `Tropical.mango` of `Fruits`).
//! Members used in their own file are marked so: `E.a`, `E['a']`, `N.a.b`
//! in values and types, a name used unqualified inside its namespace, and
//! `E[key]` or `Object.keys(E)`, which use every member. References are
//! resolved by scope, so a parameter that shadows `E` doesn't count.

use std::collections::{HashMap, HashSet};

use oxc_ast::ast::{
    BindingIdentifier, CallExpression, ComputedMemberExpression, Declaration, IdentifierReference,
    Program, Statement, StaticMemberExpression, TSEnumDeclaration, TSEnumMemberName,
    TSModuleDeclaration, TSModuleDeclarationBody, TSModuleDeclarationName, TSQualifiedName,
};
use oxc_ast_visit::{Visit, walk};
use oxc_semantic::{Scoping, SemanticBuilder};
use oxc_span::GetSpan;
use oxc_syntax::symbol::SymbolId;

use crate::exports::{Export, ExportForm, ExportKind, comment_tags};
use crate::uses::{enumerates, member_chain, member_key, qualified_chain};

/// A member of an exported enum or namespace.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ExportMember {
    /// The member's name, or for a member of a nested namespace its path
    /// from the export, as `["Tropical", "mango"]`.
    pub path: Vec<String>,
    /// [`ExportKind::EnumMember`] for an enum's key, or the kind of what a
    /// namespace exports.
    pub kind: ExportKind,
    /// 1-based line of the member's name.
    pub line: u32,
    /// Tags of the block comment right before the member, without the `@`.
    pub tags: Vec<String>,
    /// Whether the member is used in its own file.
    pub used_in_file: bool,
}

impl ExportMember {
    /// Whether the comment on the member has `@tag`.
    #[must_use]
    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags.iter().any(|t| t == tag)
    }

    /// The path joined with `.`, as `Tropical.mango`.
    #[must_use]
    pub fn name(&self) -> String {
        self.path.join(".")
    }
}

/// Fills in [`Export::members`] for the enums and namespaces `program`
/// exports. `line` maps a byte offset to its 1-based line.
pub(crate) fn collect<'a>(
    program: &'a Program<'a>,
    line: &dyn Fn(u32) -> u32,
    exports: &mut [Export],
) {
    // Declarations of the same name merge, as an enum and a namespace.
    let mut members: HashMap<&str, Vec<ExportMember>> = HashMap::new();
    for stmt in &program.body {
        let declaration = match stmt {
            Statement::ExportNamedDeclaration(decl) => decl.declaration.as_ref(),
            other => other.as_declaration(),
        };
        match declaration {
            Some(Declaration::TSEnumDeclaration(e)) => {
                members
                    .entry(e.id.name.as_str())
                    .or_default()
                    .extend(enum_members(program, e, line));
            }
            // Everything in an ambient namespace is exported whether or not
            // it says so, so its members are left alone.
            Some(Declaration::TSModuleDeclaration(m))
                if !m.declare && !program.source_type.is_typescript_definition() =>
            {
                if let TSModuleDeclarationName::Identifier(id) = &m.id {
                    let out = members.entry(id.name.as_str()).or_default();
                    namespace_members(program, m, &[], line, out);
                }
            }
            _ => {}
        }
    }
    members.retain(|_, m| !m.is_empty());
    if members.is_empty() {
        return;
    }
    // Members go to one export per local: the enum when an enum and a
    // namespace merge, as knip reports them as enum members.
    let mut owner: HashMap<&str, usize> = HashMap::new();
    for (i, e) in exports.iter().enumerate() {
        let Some(local) = e.local.as_deref() else { continue };
        // As in knip, only `export enum` and `export namespace` have
        // members checked, not `export { E }`.
        if e.form != ExportForm::Declaration
            || !matches!(e.kind, ExportKind::Enum | ExportKind::Namespace)
        {
            continue;
        }
        let Some((&key, _)) = members.get_key_value(local) else { continue };
        match owner.get(key) {
            Some(&j) if exports[j].kind == ExportKind::Enum || e.kind != ExportKind::Enum => {}
            _ => {
                owner.insert(key, i);
            }
        }
    }
    if owner.is_empty() {
        return;
    }
    let used = used_in_file(program, &members);
    for (local, i) in owner {
        let Some(mut list) = members.remove(local) else { continue };
        if let Some(u) = used.get(local) {
            for m in &mut list {
                m.used_in_file = u.covers(&m.path);
            }
        }
        exports[i].members = list;
    }
}

fn enum_members(
    program: &Program<'_>,
    e: &TSEnumDeclaration<'_>,
    line: &dyn Fn(u32) -> u32,
) -> Vec<ExportMember> {
    e.body
        .members
        .iter()
        .filter_map(|m| {
            let (name, at) = match &m.id {
                TSEnumMemberName::Identifier(id) => (id.name.as_str(), id.span.start),
                TSEnumMemberName::String(s) | TSEnumMemberName::ComputedString(s) => {
                    (s.value.as_str(), s.span.start)
                }
                TSEnumMemberName::ComputedTemplateString(t) => {
                    (t.single_quasi()?.as_str(), t.span.start)
                }
            };
            Some(ExportMember {
                path: vec![name.to_string()],
                kind: ExportKind::EnumMember,
                line: line(at),
                tags: comment_tags(program, m.span.start),
                used_in_file: false,
            })
        })
        .collect()
}

/// Adds what namespace `m` exports to `out`, each name after `prefix`.
fn namespace_members(
    program: &Program<'_>,
    m: &TSModuleDeclaration<'_>,
    prefix: &[String],
    line: &dyn Fn(u32) -> u32,
    out: &mut Vec<ExportMember>,
) {
    // Index in `out` of each path, so overloads merge into one member.
    let mut index: HashMap<Vec<String>, usize> =
        out.iter().enumerate().map(|(i, m)| (m.path.clone(), i)).collect();
    for_each_member(m, prefix, &mut |at, id, kind, path| {
        // A nested namespace isn't a member; its members are.
        if kind == ExportKind::Namespace {
            return;
        }
        let tags = comment_tags(program, at);
        if let Some(&i) = index.get(&path) {
            // Overloads are one member, tagged by any of them.
            out[i].tags.extend(tags);
            return;
        }
        index.insert(path.clone(), out.len());
        out.push(ExportMember { path, kind, line: line(id.span.start), tags, used_in_file: false });
    });
}

/// Calls `f` with each name namespace `m` exports, nested namespaces
/// included, and its path after `prefix`: where its statement starts, the
/// binding, its kind (`Namespace` for a nested namespace) and the path.
fn for_each_member<'d, 'a>(
    m: &'d TSModuleDeclaration<'a>,
    prefix: &[String],
    f: &mut dyn FnMut(u32, &'d BindingIdentifier<'a>, ExportKind, Vec<String>),
) {
    let path = |id: &BindingIdentifier<'_>| {
        let mut path = prefix.to_vec();
        path.push(id.name.to_string());
        path
    };
    let statements = match &m.body {
        Some(TSModuleDeclarationBody::TSModuleBlock(block)) => &block.body,
        // `namespace A.B {}`: `B` is a namespace inside `A`.
        Some(TSModuleDeclarationBody::TSModuleDeclaration(inner)) => {
            if let TSModuleDeclarationName::Identifier(id) = &inner.id {
                f(inner.span.start, id, ExportKind::Namespace, path(id));
                for_each_member(inner, &path(id), f);
            }
            return;
        }
        None => return,
    };
    for stmt in statements {
        let Statement::ExportNamedDeclaration(decl) = stmt else { continue };
        let Some(declaration) = &decl.declaration else { continue };
        if let Declaration::TSModuleDeclaration(inner) = declaration {
            if let TSModuleDeclarationName::Identifier(id) = &inner.id {
                f(stmt.span().start, id, ExportKind::Namespace, path(id));
                for_each_member(inner, &path(id), f);
            }
            continue;
        }
        for (id, kind) in declared(declaration) {
            f(stmt.span().start, id, kind, path(id));
        }
    }
}

/// The names a declaration inside a namespace binds, with their kinds.
fn declared<'d, 'a>(
    declaration: &'d Declaration<'a>,
) -> Vec<(&'d BindingIdentifier<'a>, ExportKind)> {
    match declaration {
        Declaration::VariableDeclaration(var) => var
            .declarations
            .iter()
            .flat_map(|d| d.id.get_binding_identifiers())
            .map(|id| (id, ExportKind::Value))
            .collect(),
        Declaration::FunctionDeclaration(f) => {
            f.id.iter().map(|id| (id, ExportKind::Function)).collect()
        }
        Declaration::ClassDeclaration(c) => c.id.iter().map(|id| (id, ExportKind::Class)).collect(),
        Declaration::TSTypeAliasDeclaration(t) => vec![(&t.id, ExportKind::Type)],
        Declaration::TSInterfaceDeclaration(i) => vec![(&i.id, ExportKind::Interface)],
        Declaration::TSEnumDeclaration(e) => vec![(&e.id, ExportKind::Enum)],
        Declaration::TSImportEqualsDeclaration(i) => vec![(&i.id, ExportKind::Value)],
        Declaration::TSModuleDeclaration(_) => Vec::new(),
    }
}

/// The member paths of an export used in its file.
#[derive(Debug, Default)]
struct UsedPaths {
    /// Paths read, and every path that contains one: reading `a.b` reads
    /// `a` too.
    paths: HashSet<Vec<String>>,
    /// Paths whose every member is used, empty for the whole export.
    whole: Vec<Vec<String>>,
}

impl UsedPaths {
    fn read(&mut self, path: &[String]) {
        for end in 1..=path.len() {
            if !self.paths.contains(&path[..end]) {
                self.paths.insert(path[..end].to_vec());
            }
        }
    }

    /// Whether the member at `path` is used: read itself or through one of
    /// its own members, or enumerated with what contains it.
    fn covers(&self, path: &[String]) -> bool {
        self.paths.contains(path) || self.whole.iter().any(|w| path.starts_with(w))
    }
}

/// Which members of the exports in `members` their file uses.
fn used_in_file<'a>(
    program: &'a Program<'a>,
    members: &HashMap<&str, Vec<ExportMember>>,
) -> HashMap<String, UsedPaths> {
    let scoping = SemanticBuilder::new().build(program).semantic.into_scoping();
    // Each symbol a use can start from: the export, a namespace nested in
    // it, or a member, by the export's local and the path to the symbol.
    let mut symbols: HashMap<SymbolId, (String, Vec<String>)> = HashMap::new();
    for stmt in &program.body {
        let declaration = match stmt {
            Statement::ExportNamedDeclaration(decl) => decl.declaration.as_ref(),
            other => other.as_declaration(),
        };
        let (id, module) = match declaration {
            Some(Declaration::TSEnumDeclaration(e)) => (&e.id, None),
            Some(Declaration::TSModuleDeclaration(m)) => match &m.id {
                TSModuleDeclarationName::Identifier(id) => (id, Some(&**m)),
                TSModuleDeclarationName::StringLiteral(_) => continue,
            },
            _ => continue,
        };
        if !members.contains_key(id.name.as_str()) {
            continue;
        }
        let local = id.name.to_string();
        if let Some(symbol) = id.symbol_id.get() {
            symbols.insert(symbol, (local.clone(), Vec::new()));
        }
        if let Some(m) = module {
            for_each_member(m, &[], &mut |_, id, _, path| {
                if let Some(symbol) = id.symbol_id.get() {
                    symbols.insert(symbol, (local.clone(), path));
                }
            });
        }
    }
    let mut finder = InFileUses { scoping: &scoping, symbols: &symbols, used: HashMap::new() };
    finder.visit_program(program);
    finder.used
}

struct InFileUses<'s> {
    scoping: &'s Scoping,
    symbols: &'s HashMap<SymbolId, (String, Vec<String>)>,
    used: HashMap<String, UsedPaths>,
}

impl InFileUses<'_> {
    /// The export and path `id` refers to, if it is one of [`Self::symbols`].
    fn resolve(&self, id: &IdentifierReference<'_>) -> Option<&(String, Vec<String>)> {
        let reference = id.reference_id.get()?;
        let symbol = self.scoping.get_reference(reference).symbol_id()?;
        self.symbols.get(&symbol)
    }

    /// Records a read of `path` below what `id` refers to; `whole` when every
    /// member below it is used.
    fn record(&mut self, id: &IdentifierReference<'_>, path: &[&str], whole: bool) -> bool {
        let Some((local, prefix)) = self.resolve(id) else { return false };
        let mut full = prefix.clone();
        full.extend(path.iter().map(|p| (*p).to_string()));
        let used = self.used.entry(local.clone()).or_default();
        if whole {
            if !used.whole.contains(&full) {
                used.whole.push(full);
            }
        } else {
            used.read(&full);
        }
        true
    }
}

impl<'a> Visit<'a> for InFileUses<'_> {
    fn visit_identifier_reference(&mut self, it: &IdentifierReference<'a>) {
        // A member used by its own name inside its namespace. The export
        // itself used as a value uses none of its members, as in knip.
        self.record(it, &[], false);
    }

    fn visit_static_member_expression(&mut self, it: &StaticMemberExpression<'a>) {
        if let Some((root, mut path)) = member_chain(&it.object) {
            path.push(&it.property.name);
            if self.record(root, &path, false) {
                return;
            }
        }
        walk::walk_static_member_expression(self, it);
    }

    fn visit_computed_member_expression(&mut self, it: &ComputedMemberExpression<'a>) {
        if let Some((root, mut path)) = member_chain(&it.object) {
            match member_key(&it.expression) {
                Some(key) => {
                    path.push(key);
                    if self.record(root, &path, false) {
                        return;
                    }
                }
                // `E[key]` reads any member.
                None => {
                    if self.record(root, &path, true) {
                        return self.visit_expression(&it.expression);
                    }
                }
            }
        }
        walk::walk_computed_member_expression(self, it);
    }

    fn visit_ts_qualified_name(&mut self, it: &TSQualifiedName<'a>) {
        if let Some((root, path)) = qualified_chain(it)
            && self.record(root, &path, false)
        {
            return;
        }
        walk::walk_ts_qualified_name(self, it);
    }

    fn visit_call_expression(&mut self, it: &CallExpression<'a>) {
        if enumerates(it) {
            for arg in &it.arguments {
                if let Some(expr) = arg.as_expression()
                    && let Some((root, path)) = member_chain(expr)
                {
                    self.record(root, &path, true);
                }
            }
        }
        walk::walk_call_expression(self, it);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxc_allocator::Allocator;
    use oxc_parser::Parser;
    use oxc_span::SourceType;

    /// Each export's members as `export.path`, with `*` when used in the
    /// file.
    fn members(src: &str, source_type: SourceType) -> Vec<String> {
        let allocator = Allocator::default();
        let ret = Parser::new(&allocator, src, source_type).parse();
        assert!(ret.errors.is_empty(), "{:?}", ret.errors);
        let program = &*allocator.alloc(ret.program);
        let mut exports = crate::exports::collect(program, &|_| 1);
        collect(program, &|_| 1, &mut exports);
        exports
            .iter()
            .flat_map(|e| {
                e.members.iter().map(move |m| {
                    let used = if m.used_in_file { "*" } else { "" };
                    format!("{}.{}{used}", e.name, m.name())
                })
            })
            .collect()
    }

    fn ts(src: &str) -> Vec<String> {
        members(src, SourceType::ts())
    }

    #[test]
    fn enum_keys() {
        let src = "export enum E { A = 1, 'b-c' = 2, ['d'] = 3 }
const x: E.A = 1;
enum Local { X }";
        assert_eq!(ts(src), ["E.A*", "E.b-c", "E.d"]);
    }

    #[test]
    fn namespace_exports_flattened() {
        let src = "export namespace N {
  export const a = 1, b = 2;
  const hidden = 3;
  export function f(x: number): number;
  export function f(x: string): number;
  export function f(x: any) { return 1; }
  export class C {}
  export type T = string;
  export interface I {}
  export enum Inner { Z }
  export namespace Deep { export const d = 1; }
}
export namespace A.B { export const c = 1; }";
        assert_eq!(
            ts(src),
            ["N.a", "N.b", "N.f", "N.C", "N.T", "N.I", "N.Inner", "N.Deep.d", "A.B.c"]
        );
    }

    #[test]
    fn uses_in_the_same_file() {
        let src = "export namespace N {
  export const a = 1;
  export const b = a + 1;
  export namespace Deep { export const d = 1; export const e = 2; }
  export const f = Deep.d;
}
export namespace M { export const x = 1; export const y = 2; }
export enum E { A, B }
export enum F { A, B }
console.log(M.x, E[code], M);
Object.keys(F);";
        assert_eq!(
            ts(src),
            [
                "N.a*",
                "N.b",
                "N.Deep.d*",
                "N.Deep.e",
                "N.f",
                "M.x*",
                "M.y",
                "E.A*",
                "E.B*",
                "F.A*",
                "F.B*"
            ]
        );
    }

    #[test]
    fn shadowed_names_are_not_uses() {
        let src = "function g(N: { a: number }) { return N.a; }
const h = (E: { A: number }) => E.A;
export namespace N { export const a = 1; }
export enum E { A }";
        assert_eq!(ts(src), ["N.a", "E.A"]);
    }

    #[test]
    fn merged_enum_and_namespace() {
        let src = "export enum Status { Active, Inactive }
export namespace Status {
  export function label(s: Status) { return s === Status.Active ? 'a' : 'i'; }
  export const unused = 0;
}";
        assert_eq!(ts(src), ["Status.Active*", "Status.Inactive", "Status.label", "Status.unused"]);
    }

    #[test]
    fn exported_by_name_and_ambient() {
        // knip checks only enums and namespaces exported where declared.
        assert!(ts("enum E { A } export { E as F };").is_empty());
        // Everything in an ambient namespace is exported, so it is left alone.
        assert!(ts("export declare namespace N { const a: number; }").is_empty());
        assert!(members("export namespace N { const a: number; }", SourceType::d_ts()).is_empty());
    }
}
