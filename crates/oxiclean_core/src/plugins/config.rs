//! Reading tools' config files without running them. A JSON or JSONC
//! file is parsed; a JS/TS file's exported object literal is read as far
//! as it is plain data. Strings, numbers, booleans, arrays and objects
//! become their JSON values, a `defineConfig({...})`-style call becomes its
//! first argument, and a top-level `const` becomes its value. Anything else,
//! such as `require('x')`, is `null`, as its import already counts.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex},
};

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    ArrayExpressionElement, ExportDefaultDeclarationKind, Expression, FunctionBody,
    ObjectPropertyKind, Statement,
};
use oxc_parser::Parser;
use oxc_span::SourceType;
use serde_json::{Map, Value};

use crate::jsonc::parse_jsonc;

/// How deep a value is read, so that constants naming each other end.
const MAX_DEPTH: usize = 32;

/// The config in `file`: JSON(C) unless its extension is a JS/TS one.
pub(super) fn load(file: &Path) -> Option<Value> {
    let src = fs::read_to_string(file).ok()?;
    let ext = file.extension().and_then(|e| e.to_str()).unwrap_or_default();
    match ext {
        "js" | "mjs" | "cjs" | "ts" | "mts" | "cts" => {
            from_js(&src, SourceType::from_path(file).ok()?)
        }
        _ => parse_jsonc(src.trim_start_matches('\u{feff}')),
    }
}

/// `field` of the package.json in `dir`, as `eslintConfig` or `babel`.
pub(super) fn package_field(dir: &Path, field: &str) -> Option<Value> {
    let src = fs::read_to_string(dir.join("package.json")).ok()?;
    parse_jsonc(src.trim_start_matches('\u{feff}'))?.get(field).cloned()
}

/// The value a JS/TS module exports with `export default` or
/// `module.exports =`. A function, as Babel's `(api) => ({...})`, exports
/// what its body returns.
fn from_js(src: &str, source_type: SourceType) -> Option<Value> {
    let allocator = Allocator::default();
    let program = Parser::new(&allocator, src, source_type).parse().program;
    let mut consts = HashMap::new();
    declare(&program.body, &mut consts);
    let mut exported = None;
    for stmt in &program.body {
        match stmt {
            Statement::ExportDefaultDeclaration(decl) => {
                exported = match &decl.declaration {
                    ExportDefaultDeclarationKind::FunctionDeclaration(f) => {
                        f.body.as_deref().and_then(|b| returned(b, &mut consts))
                    }
                    kind => kind.as_expression(),
                };
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
    let mut exported = exported?;
    // `export default config` where `config` is a function.
    if let Expression::Identifier(id) = exported.get_inner_expression()
        && let Some(e) = consts.get(id.name.as_str())
    {
        exported = e;
    }
    match exported.get_inner_expression() {
        Expression::ArrowFunctionExpression(a) if a.expression => {
            declare(&a.body.statements, &mut consts);
            exported = a.get_expression()?;
        }
        Expression::ArrowFunctionExpression(a) => exported = returned(&a.body, &mut consts)?,
        Expression::FunctionExpression(f) => {
            exported = returned(f.body.as_deref()?, &mut consts)?;
        }
        _ => {}
    }
    Some(Reader { consts }.value(exported, 0))
}

/// Records the `const`s, `let`s and `var`s that `stmts` declare.
fn declare<'s, 'a>(stmts: &'s [Statement<'a>], consts: &mut HashMap<&'s str, &'s Expression<'a>>) {
    for stmt in stmts {
        if let Statement::VariableDeclaration(decl) = stmt {
            for d in &decl.declarations {
                if let (Some(name), Some(init)) = (d.id.get_identifier_name(), &d.init) {
                    consts.insert(name.as_str(), init);
                }
            }
        }
    }
}

/// What a function `body` returns last, with its own declarations
/// recorded.
fn returned<'s, 'a>(
    body: &'s FunctionBody<'a>,
    consts: &mut HashMap<&'s str, &'s Expression<'a>>,
) -> Option<&'s Expression<'a>> {
    declare(&body.statements, consts);
    body.statements.iter().rev().find_map(|s| match s {
        Statement::ReturnStatement(r) => r.argument.as_ref(),
        _ => None,
    })
}

struct Reader<'s, 'a> {
    consts: HashMap<&'s str, &'s Expression<'a>>,
}

impl<'a> Reader<'_, 'a> {
    fn value(&self, expr: &Expression<'a>, depth: usize) -> Value {
        if depth > MAX_DEPTH {
            return Value::Null;
        }
        match expr.get_inner_expression() {
            Expression::StringLiteral(s) => Value::String(s.value.to_string()),
            Expression::TemplateLiteral(t) => {
                t.single_quasi().map_or(Value::Null, |s| Value::String(s.to_string()))
            }
            Expression::BooleanLiteral(b) => Value::Bool(b.value),
            Expression::NumericLiteral(n) => {
                serde_json::Number::from_f64(n.value).map_or(Value::Null, Value::Number)
            }
            Expression::ArrayExpression(a) => Value::Array(
                a.elements
                    .iter()
                    .filter_map(ArrayExpressionElement::as_expression)
                    .map(|e| self.value(e, depth + 1))
                    .collect(),
            ),
            Expression::ObjectExpression(o) => {
                let mut map = Map::new();
                for p in &o.properties {
                    match p {
                        ObjectPropertyKind::ObjectProperty(p) => {
                            if let Some(key) = p.key.static_name() {
                                map.insert(key.into_owned(), self.value(&p.value, depth + 1));
                            }
                        }
                        // `...base`: its keys, when it is a constant object.
                        ObjectPropertyKind::SpreadProperty(s) => {
                            if let Value::Object(base) = self.value(&s.argument, depth + 1) {
                                for (k, v) in base {
                                    map.entry(k).or_insert(v);
                                }
                            }
                        }
                    }
                }
                Value::Object(map)
            }
            // `require('x')` is an import, not data.
            Expression::CallExpression(c) if c.is_require_call() => Value::Null,
            Expression::CallExpression(c) => c
                .arguments
                .first()
                .and_then(|a| a.as_expression())
                .map_or(Value::Null, |e| self.value(e, depth + 1)),
            Expression::Identifier(id) => {
                self.consts.get(id.name.as_str()).map_or(Value::Null, |e| self.value(e, depth + 1))
            }
            _ => Value::Null,
        }
    }
}

/// The strings in `value`: itself, or each string of an array, or the
/// first item of each `[name, options]` pair in one, as ESLint, Babel and
/// PostCSS take plugins.
pub(super) fn names(value: Option<&Value>) -> Vec<&str> {
    match value {
        Some(Value::String(s)) => vec![s],
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|i| match i {
                Value::String(s) => Some(s.as_str()),
                Value::Array(pair) => pair.first()?.as_str(),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// The package a short name stands for under a tool's naming convention,
/// as ESLint and Babel resolve them: with `prefix` `eslint-config`,
/// `airbnb` is `eslint-config-airbnb`, `@scope` is `@scope/eslint-config`
/// and `@scope/x` is `@scope/eslint-config-x`. A subpath is kept, and a
/// name that already has the prefix is the package itself.
pub fn prefixed(name: &str, prefix: &str) -> String {
    let (scope, rest) = match name.strip_prefix('@').and_then(|n| n.split_once('/')) {
        Some((scope, rest)) => (Some(scope), rest),
        None if name.starts_with('@') => return format!("{name}/{prefix}"),
        None => (None, name),
    };
    let has_prefix = rest == prefix
        || rest.starts_with(&format!("{prefix}-"))
        || rest.starts_with(&format!("{prefix}/"));
    let rest = if has_prefix { rest.to_string() } else { format!("{prefix}-{rest}") };
    match scope {
        Some(scope) => format!("@{scope}/{rest}"),
        None => rest,
    }
}

/// Extensions tried for a file a config names without one.
const SOURCE_EXTENSIONS: &[&str] =
    &["", ".ts", ".tsx", ".js", ".jsx", ".mts", ".mjs", ".cts", ".cjs"];

/// Sorts what a config names into packages and files: `<rootDir>/setup.ts`
/// and `./setup` are files in `dir`, anything else a package.
#[derive(Debug, Default)]
pub(super) struct Named {
    pub packages: Vec<String>,
    pub files: Vec<PathBuf>,
}

impl Named {
    pub(super) fn add(&mut self, dir: &Path, name: &str) {
        let name = name.replace("<rootDir>", ".");
        if is_path(&name) {
            let base = dir.join(&name);
            let found = SOURCE_EXTENSIONS
                .iter()
                .map(|ext| PathBuf::from(format!("{}{ext}", base.display())))
                .find(|p| p.is_file());
            self.files.extend(found.map(|p| p.canonicalize().unwrap_or(p)));
        } else if !name.is_empty() {
            self.packages.push(name);
        }
    }

    pub(super) fn add_all(&mut self, dir: &Path, value: Option<&Value>) {
        for name in names(value) {
            self.add(dir, name);
        }
    }
}

/// Whether the package.json in `dir` lists `package` in any dependency
/// field. Cached, as route plugins ask once per file.
pub(super) fn lists_package(dir: &Path, package: &str) -> bool {
    static CACHE: LazyLock<Mutex<HashMap<(PathBuf, String), bool>>> = LazyLock::new(Mutex::default);
    let key = (dir.to_path_buf(), package.to_string());
    if let Some(&hit) = CACHE.lock().ok().as_ref().and_then(|c| c.get(&key)) {
        return hit;
    }
    let listed = fs::read_to_string(dir.join("package.json"))
        .ok()
        .and_then(|s| parse_jsonc(s.trim_start_matches('\u{feff}')))
        .is_some_and(|pkg| {
            ["dependencies", "devDependencies", "peerDependencies", "optionalDependencies"]
                .iter()
                .any(|f| pkg.get(f).and_then(|d| d.get(package)).is_some())
        });
    if let Ok(mut c) = CACHE.lock() {
        c.insert(key, listed);
    }
    listed
}

/// The package directory that owns a routes directory such as `app`: its
/// parent, or the parent of a `src` parent, whichever has a package.json.
pub(super) fn routes_owner(routes: &Path) -> Option<&Path> {
    let parent = routes.parent()?;
    if parent.join("package.json").is_file() {
        return Some(parent);
    }
    let grand = parent.parent()?;
    (parent.file_name()? == "src" && grand.join("package.json").is_file()).then_some(grand)
}

/// Whether `name` is a path rather than a package.
pub(super) fn is_path(name: &str) -> bool {
    name.starts_with('.') || name.starts_with('/') || Path::new(name).is_absolute()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn js(src: &str) -> Value {
        from_js(src, SourceType::mjs()).unwrap()
    }

    #[test]
    fn reads_exported_literals() {
        assert_eq!(
            js(r"
              const base = { plugins: ['a'], n: 1 };
              const presets = [['@babel/env', { loose: true }], `react`];
              export default defineConfig({ ...base, presets, x: require('y'), t: `${a}` } as const);
            "),
            json!({
              "plugins": ["a"], "n": 1.0,
              "presets": [["@babel/env", { "loose": true }], "react"],
              "x": null, "t": null,
            })
        );
        assert_eq!(js("module.exports = { extends: 'airbnb' };"), json!({ "extends": "airbnb" }));
        let babel = json!({ "presets": ["a"], "plugins": ["b"] });
        let functions = [
            "export default function config(api) { const plugins = ['b']; return { presets: ['a'], plugins }; }",
            "module.exports = (api) => ({ presets: ['a'], plugins: ['b'] });",
            "module.exports = function (api) { api.cache(true); return { presets: ['a'], plugins: ['b'] }; };",
            "const config = api => { return { presets: ['a'], plugins: ['b'] }; }; export default config;",
        ];
        for src in functions {
            assert_eq!(js(src), babel, "{src}");
        }
        assert_eq!(names(Some(&json!(["a", ["b", {}], 3]))), ["a", "b"]);
    }

    #[test]
    fn self_reference_ends() {
        assert!(js("const a = [a]; export default a;").is_array());
    }

    #[test]
    fn prefixes() {
        let p = |n| prefixed(n, "eslint-config");
        assert_eq!(p("airbnb"), "eslint-config-airbnb");
        assert_eq!(p("airbnb/hooks"), "eslint-config-airbnb/hooks");
        assert_eq!(p("eslint-config-prettier"), "eslint-config-prettier");
        assert_eq!(p("@acme"), "@acme/eslint-config");
        assert_eq!(p("@acme/web"), "@acme/eslint-config-web");
        assert_eq!(p("@acme/eslint-config"), "@acme/eslint-config");
        assert_eq!(p("@acme/eslint-config/strict"), "@acme/eslint-config/strict");
    }
}
