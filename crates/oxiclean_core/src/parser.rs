use anyhow::{Context, Result};
use dashmap::DashMap;
use oxc_allocator::Allocator;
use oxc_ast::ast::*;
use oxc_ast_visit::{Visit, walk};
use oxc_parser::{Parser as OxcParser, ParserReturn};
use oxc_span::{SourceType, Span};
use std::{
    fs,
    path::{Path, PathBuf},
};
use tracing::{debug, trace, warn};

use crate::constants::{JS_TS_EXTENSIONS, is_declaration};
use crate::exports::{self, Export};
use crate::globs;
use crate::members;
use crate::plugins::Plugins;
use crate::types::{SpecKind, Specifier};
use crate::uses::{self, ModuleUses, Usage, Used};

/// The runtime imports of `file`, cached in `cache`: type-only imports are
/// left out, so a declaration file has none. Assets such as `.css` have
/// none either. Syntax errors are logged, and the imports found before them
/// returned.
///
/// # Errors
/// Fails if the file can't be read.
pub fn imports_for(
    file: &Path,
    cache: &DashMap<PathBuf, Vec<Specifier>>,
) -> Result<Vec<Specifier>> {
    let file_buf = file.to_path_buf();
    if let Some(v) = cache.get(&file_buf) {
        trace!("Cache hit for imports: {}", file.display());
        return Ok(v.clone());
    }

    // Assets such as `.css` or `.json` are leaves of the graph
    let is_js =
        file.extension().and_then(|e| e.to_str()).is_some_and(|e| JS_TS_EXTENSIONS.contains(&e));
    if !is_js {
        cache.insert(file_buf, Vec::new());
        return Ok(Vec::new());
    }

    let parsed = parse_imports(file)?;
    if let Some(error) = &parsed.error {
        warn!("{} has syntax errors; imports may be incomplete: {error}", file.display());
    }
    let specs: Vec<Specifier> =
        parsed.specs.into_iter().filter(|s| s.kind != SpecKind::Type).collect();
    cache.insert(file_buf, specs.clone());
    Ok(specs)
}

/// The imports and exports of one file, plus the first syntax error if the
/// file did not parse cleanly. After a syntax error `specs`, `exports` and
/// `uses` are incomplete, usually empty.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ParsedImports {
    /// Every module the file imports or re-exports from, type-only ones
    /// included as [`SpecKind::Type`].
    pub specs: Vec<Specifier>,
    pub exports: Vec<Export>,
    /// What the file takes from the modules it imports.
    pub uses: ModuleUses,
    pub error: Option<String>,
}

/// Reads and parses `file`, returning every import it makes and every name
/// it exports.
///
/// Unlike [`imports_for`] this does not cache, keeps type-only imports, and
/// reports syntax errors instead of silently returning a partial list.
///
/// # Errors
/// Fails if the file can't be read.
pub fn parse_imports(file: &Path) -> Result<ParsedImports> {
    parse_file(file, &Plugins::default(), true, true)
}

/// [`parse_imports`], leaving [`ParsedImports::exports`] and
/// [`ParsedImports::uses`] empty unless `with_exports`, and
/// [`Export::members`] empty unless `with_members` too: checks that only
/// follow imports skip the extra walks over the AST. Files of formats
/// other than JS/TS are read by the plugin in `plugins` that knows them,
/// and have no imports when it is off.
pub(crate) fn parse_file(
    file: &Path,
    plugins: &Plugins,
    with_exports: bool,
    with_members: bool,
) -> Result<ParsedImports> {
    trace!("Parsing file for imports: {}", file.display());
    let src =
        fs::read_to_string(file).with_context(|| format!("Failed to read {}", file.display()))?;

    let ext = file.extension().and_then(|e| e.to_str()).unwrap_or_default();
    let (src, st, extra) = if JS_TS_EXTENSIONS.contains(&ext) {
        (src, source_type_for(file), Vec::new())
    } else if let Some(extracted) = plugins.for_file(file).and_then(|p| p.extract(file, &src)) {
        (extracted.js, extracted.source_type, extracted.imports)
    } else {
        return Ok(ParsedImports::default());
    };
    let allocator = Allocator::default();
    let ParserReturn { program, errors, panicked, .. } =
        OxcParser::new(&allocator, &src, st).parse();

    let mut collector = ImportCollector::new(&src);
    for (request, offset) in &extra {
        let span = Span::new(u32::try_from(*offset).unwrap_or(u32::MAX), 0);
        collector.push(request, SpecKind::Static, span);
    }
    collector.visit_program(&program);
    collector.reference_types(&program.comments, &src);
    // What a glob imports is read through the module objects it returns,
    // so every export counts as used, or the one its `import` option names.
    let mut glob_uses = Vec::new();
    for glob in std::mem::take(&mut collector.globs) {
        for request in globs::expand(file, &glob.patterns) {
            collector.push(&request, glob.kind, glob.span);
            let used = glob.import.clone().map_or(Used::All, Used::Name);
            glob_uses.push(Usage { request, used });
        }
    }
    let (exports, uses) = if with_exports {
        let line = |offset| collector.line(offset);
        let mut exports = exports::collect(&program, &line);
        // Scopes are resolved on the arena's copy of the program.
        let program = &*allocator.alloc(program);
        if with_members {
            members::collect(program, &line, &mut exports);
        }
        let mut uses = uses::collect(program);
        uses.usages.extend(glob_uses);
        (exports, uses)
    } else {
        (Vec::new(), ModuleUses::default())
    };
    let mut specs = collector.specs;
    // A declaration file only describes types, so its imports load nothing.
    if is_declaration(file) {
        for spec in &mut specs {
            spec.kind = SpecKind::Type;
        }
    }

    let error = if panicked && errors.is_empty() {
        Some("parser gave up".to_string())
    } else {
        errors.first().map(|e| e.message.to_string())
    };
    debug!("Found {} import specifiers in {}", specs.len(), file.display());
    Ok(ParsedImports { specs, exports, uses, error })
}

/// Walks the whole AST collecting module requests, including re-exports and
/// `require()`/`import()` calls nested inside functions or conditionals.
struct ImportCollector {
    specs: Vec<Specifier>,
    /// `import.meta.glob` calls, expanded once the walk is done.
    globs: Vec<Glob>,
    /// Byte offset at which each line starts, for turning spans into lines.
    line_starts: Vec<u32>,
}

impl ImportCollector {
    fn new(src: &str) -> Self {
        let line_starts = std::iter::once(0)
            .chain(src.match_indices('\n').map(|(i, _)| u32::try_from(i + 1).unwrap_or(u32::MAX)))
            .collect();
        Self { specs: Vec::new(), globs: Vec::new(), line_starts }
    }

    /// 1-based line of the byte at `offset`.
    fn line(&self, offset: u32) -> u32 {
        let line = self.line_starts.partition_point(|&start| start <= offset);
        u32::try_from(line).unwrap_or(u32::MAX)
    }

    fn push(&mut self, request: &str, kind: SpecKind, span: Span) {
        trace!("Found {:?} import: '{}'", kind, request);
        let line = self.line(span.start);
        self.specs.push(Specifier { request: request.to_string(), kind, line });
    }
}

impl ImportCollector {
    /// `/// <reference types="x" />` loads `x`'s types: a type-only import.
    fn reference_types(&mut self, comments: &[Comment], src: &str) {
        for comment in comments.iter().filter(|c| c.is_line()) {
            let span = comment.span;
            let Some(text) = src.get(span.start as usize..span.end as usize) else { continue };
            let Some(rest) = text.strip_prefix("///") else { continue };
            let Some(rest) = rest.trim_start().strip_prefix("<reference") else { continue };
            let Some(at) = rest.find("types=") else { continue };
            let value = &rest[at + "types=".len()..];
            let Some(quote) = value.chars().next().filter(|c| matches!(c, '"' | '\'')) else {
                continue;
            };
            if let Some(end) = value[1..].find(quote) {
                self.push(&value[1..=end], SpecKind::Type, span);
            }
        }
    }
}

impl<'a> Visit<'a> for ImportCollector {
    fn visit_import_declaration(&mut self, decl: &ImportDeclaration<'a>) {
        if decl.import_kind.is_type() {
            self.push(&decl.source.value, SpecKind::Type, decl.span);
            return;
        }
        // `import { type Foo } from 'bar'` loads nothing when every specifier
        // is a type. No specifiers means a side-effect import: `import 'x'`.
        let has_runtime_import = decl.specifiers.as_ref().is_none_or(|specifiers| {
            specifiers.is_empty()
                || specifiers.iter().any(|spec| match spec {
                    ImportDeclarationSpecifier::ImportSpecifier(s) => !s.import_kind.is_type(),
                    _ => true,
                })
        });
        let kind = if has_runtime_import { SpecKind::Static } else { SpecKind::Type };
        self.push(&decl.source.value, kind, decl.span);
    }

    fn visit_export_all_declaration(&mut self, decl: &ExportAllDeclaration<'a>) {
        let kind = if decl.export_kind.is_type() { SpecKind::Type } else { SpecKind::Static };
        self.push(&decl.source.value, kind, decl.span);
    }

    fn visit_export_named_declaration(&mut self, decl: &ExportNamedDeclaration<'a>) {
        if let Some(source) = &decl.source {
            let has_runtime_export = !decl.export_kind.is_type()
                && (decl.specifiers.is_empty()
                    || decl.specifiers.iter().any(|s| !s.export_kind.is_type()));
            let kind = if has_runtime_export { SpecKind::Static } else { SpecKind::Type };
            self.push(&source.value, kind, decl.span);
        }
        walk::walk_export_named_declaration(self, decl);
    }

    fn visit_ts_import_equals_declaration(&mut self, decl: &TSImportEqualsDeclaration<'a>) {
        // import x = require('y')
        if let TSModuleReference::ExternalModuleReference(ext) = &decl.module_reference {
            let kind = if decl.import_kind.is_type() { SpecKind::Type } else { SpecKind::Static };
            self.push(&ext.expression.value, kind, decl.span);
        }
    }

    fn visit_import_expression(&mut self, expr: &ImportExpression<'a>) {
        if let Expression::StringLiteral(sl) = &expr.source {
            self.push(&sl.value, SpecKind::Dynamic, expr.span);
        } else if let Expression::TemplateLiteral(tl) = &expr.source
            && let Some(quasi) = tl.single_quasi()
        {
            self.push(&quasi, SpecKind::Dynamic, expr.span);
        }
        walk::walk_import_expression(self, expr);
    }

    fn visit_call_expression(&mut self, call: &CallExpression<'a>) {
        if let Expression::Identifier(callee) = &call.callee
            && callee.name == "require"
            && call.arguments.len() == 1
            && let Some(Expression::StringLiteral(sl)) = call.arguments[0].as_expression()
        {
            self.push(&sl.value, SpecKind::Static, call.span);
        } else if let Some(request) = uses::resolve_request(call) {
            // Finds a file without loading it, so nothing runs here.
            self.push(request, SpecKind::Type, call.span);
        } else if let Some(glob) = import_meta_glob(call) {
            self.globs.push(glob);
        }
        walk::walk_call_expression(self, call);
    }

    // `new URL('./worker.ts', import.meta.url)`, as `new Worker(...)` and
    // assets take, finds a file without loading it.
    fn visit_new_expression(&mut self, expr: &NewExpression<'a>) {
        if let Expression::Identifier(callee) = &expr.callee
            && callee.name == "URL"
            && let [first, second, ..] = expr.arguments.as_slice()
            && let Some(Expression::StringLiteral(url)) = first.as_expression()
            && second.as_expression().is_some_and(is_import_meta_url)
            && !url.value.contains(':')
            && !url.value.starts_with('/')
            // `new URL('.', import.meta.url)` is a directory.
            && !matches!(url.value.as_str(), "" | "." | "..")
            && !url.value.ends_with('/')
        {
            let value = url.value.as_str();
            // A URL without `./` is still relative to the file.
            let request =
                if value.starts_with('.') { value.to_string() } else { format!("./{value}") };
            self.push(&request, SpecKind::Type, expr.span);
        }
        walk::walk_new_expression(self, expr);
    }

    // `typeof import('x')` and `import('x').T` in a type refer to `x`'s
    // types: a type-only import.
    fn visit_ts_import_type(&mut self, it: &TSImportType<'a>) {
        if let TSType::TSLiteralType(lit) = &it.argument
            && let TSLiteral::StringLiteral(sl) = &lit.literal
        {
            self.push(&sl.value, SpecKind::Type, it.span);
        }
        walk::walk_ts_import_type(self, it);
    }

    // `declare module './x' { ... }` augments `./x`'s types: a type-only
    // import of it. Imports inside describe types, not runtime edges.
    fn visit_ts_module_declaration(&mut self, decl: &TSModuleDeclaration<'a>) {
        if let TSModuleDeclarationName::StringLiteral(module) = &decl.id {
            self.push(&module.value, SpecKind::Type, decl.span);
        }
    }
}

fn is_import_meta(expr: &Expression<'_>) -> bool {
    matches!(expr, Expression::MetaProperty(m) if m.meta.name == "import" && m.property.name == "meta")
}

fn is_import_meta_url(expr: &Expression<'_>) -> bool {
    matches!(expr, Expression::StaticMemberExpression(m)
        if m.property.name == "url" && is_import_meta(&m.object))
}

/// An `import.meta.glob('./pages/*.ts', { eager, import })` call.
struct Glob {
    patterns: Vec<String>,
    /// Static when eager, else dynamic, as Vite loads them.
    kind: SpecKind,
    /// The one export each module is read for, from the `import` option.
    import: Option<String>,
    span: Span,
}

fn import_meta_glob(call: &CallExpression<'_>) -> Option<Glob> {
    let Expression::StaticMemberExpression(callee) = &call.callee else { return None };
    if !is_import_meta(&callee.object)
        || !matches!(callee.property.name.as_str(), "glob" | "globEager")
    {
        return None;
    }
    let literal = |e: &Expression<'_>| match e {
        Expression::StringLiteral(s) => Some(s.value.to_string()),
        Expression::TemplateLiteral(t) => t.single_quasi().map(|q| q.to_string()),
        _ => None,
    };
    let patterns = match call.arguments.first()?.as_expression()? {
        Expression::ArrayExpression(a) => {
            a.elements.iter().filter_map(|e| e.as_expression()).filter_map(literal).collect()
        }
        e => vec![literal(e)?],
    };
    let option = |name: &str| {
        let Some(Expression::ObjectExpression(o)) = call.arguments.get(1)?.as_expression() else {
            return None;
        };
        o.properties.iter().find_map(|p| match p {
            ObjectPropertyKind::ObjectProperty(p)
                if p.key.static_name().is_some_and(|k| k == name) =>
            {
                Some(&p.value)
            }
            _ => None,
        })
    };
    let eager = callee.property.name == "globEager"
        || matches!(option("eager"), Some(Expression::BooleanLiteral(b)) if b.value);
    let import = match option("import") {
        Some(Expression::StringLiteral(s)) if s.value != "*" => Some(s.value.to_string()),
        _ => None,
    };
    let kind = if eager { SpecKind::Static } else { SpecKind::Dynamic };
    Some(Glob { patterns, kind, import, span: call.span })
}

/// How to parse `path`. JavaScript files may contain JSX whatever their
/// extension, as Next.js (SWC), Metro (Babel) and most bundlers allow, and a
/// `.js` file may be either an ES module or a CommonJS script.
fn source_type_for(path: &Path) -> SourceType {
    let st = SourceType::from_path(path).unwrap_or_default();
    if !st.is_javascript() {
        return st;
    }
    let st = st.with_jsx(true);
    match path.extension().and_then(|e| e.to_str()) {
        Some("js" | "jsx") => st.with_unambiguous(true),
        _ => st,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::SpecKind;
    use std::fs;
    use tempfile::TempDir;

    fn create_test_file(dir: &Path, name: &str, content: &str) -> PathBuf {
        let file_path = dir.join(name);
        fs::write(&file_path, content).expect("Failed to write test file");
        file_path
    }

    #[test]
    fn test_static_import_default() {
        let temp_dir = TempDir::new().unwrap();
        let cache = DashMap::new();
        let file = create_test_file(temp_dir.path(), "test.js", "import foo from './foo';");
        let imports = imports_for(&file, &cache).unwrap();
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].request, "./foo");
        assert!(matches!(imports[0].kind, SpecKind::Static));
    }

    #[test]
    fn test_import_lines() {
        let temp_dir = TempDir::new().unwrap();
        let cache = DashMap::new();
        let file = create_test_file(
            temp_dir.path(),
            "test.js",
            "// header\nimport a from './a';\n\nconst b = require('./b');\nexport * from './c';",
        );
        let lines: Vec<(String, u32)> =
            imports_for(&file, &cache).unwrap().into_iter().map(|s| (s.request, s.line)).collect();
        assert_eq!(lines, vec![("./a".into(), 2), ("./b".into(), 4), ("./c".into(), 5)]);
    }

    #[test]
    fn test_jsx_and_commonjs_in_every_js_extension() {
        let temp_dir = TempDir::new().unwrap();
        let cache = DashMap::new();
        let jsx = "import a from './a';\nexport const C = () => <div>{a}</div>;";
        for name in ["c.js", "c.mjs", "c.jsx"] {
            let file = create_test_file(temp_dir.path(), name, jsx);
            let parsed = parse_imports(&file).unwrap();
            assert_eq!(parsed.error, None, "{name}");
            assert_eq!(parsed.specs.len(), 1, "{name}");
            imports_for(&file, &cache).unwrap();
        }
        // Sloppy-mode CommonJS: `with` and octal literals are errors in modules.
        let cjs = "const a = require('./a');\nwith (a) { module.exports = 010; }";
        for name in ["s.js", "s.cjs"] {
            let file = create_test_file(temp_dir.path(), name, cjs);
            let parsed = parse_imports(&file).unwrap();
            assert_eq!(parsed.error, None, "{name}");
            assert_eq!(parsed.specs.len(), 1, "{name}");
        }
        // TypeScript keeps its own rules: `<T>x` is a cast in .ts, not JSX.
        let file =
            create_test_file(temp_dir.path(), "t.ts", "import a from './a';\nconst x = <number>a;");
        assert_eq!(parse_imports(&file).unwrap().error, None);
    }

    #[test]
    fn test_static_import_named() {
        let temp_dir = TempDir::new().unwrap();
        let cache = DashMap::new();
        let file =
            create_test_file(temp_dir.path(), "test.js", "import { bar, baz } from './utils';");
        let imports = imports_for(&file, &cache).unwrap();
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].request, "./utils");
        assert!(matches!(imports[0].kind, SpecKind::Static));
    }

    #[test]
    fn test_static_import_namespace() {
        let temp_dir = TempDir::new().unwrap();
        let cache = DashMap::new();
        let file =
            create_test_file(temp_dir.path(), "test.js", "import * as utils from './utils';");
        let imports = imports_for(&file, &cache).unwrap();
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].request, "./utils");
        assert!(matches!(imports[0].kind, SpecKind::Static));
    }

    #[test]
    fn test_side_effect_import() {
        let temp_dir = TempDir::new().unwrap();
        let cache = DashMap::new();
        let file = create_test_file(temp_dir.path(), "test.js", "import './polyfills';");
        let imports = imports_for(&file, &cache).unwrap();
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].request, "./polyfills");
        assert!(matches!(imports[0].kind, SpecKind::Static));
    }

    #[test]
    fn test_dynamic_import() {
        let temp_dir = TempDir::new().unwrap();
        let cache = DashMap::new();
        // Dynamic import() as a top-level expression statement
        let file = create_test_file(temp_dir.path(), "test.js", "import('./lazy');");
        let imports = imports_for(&file, &cache).unwrap();
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].request, "./lazy");
        assert!(matches!(imports[0].kind, SpecKind::Dynamic));
    }

    #[test]
    fn test_require_call() {
        let temp_dir = TempDir::new().unwrap();
        let cache = DashMap::new();
        let file = create_test_file(temp_dir.path(), "test.js", "const fs = require('fs');");
        let imports = imports_for(&file, &cache).unwrap();
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].request, "fs");
        assert!(matches!(imports[0].kind, SpecKind::Static));
    }

    #[test]
    fn worker_urls_and_glob_imports() {
        let temp_dir = TempDir::new().unwrap();
        fs::create_dir_all(temp_dir.path().join("pages")).unwrap();
        fs::write(temp_dir.path().join("pages/a.ts"), "").unwrap();
        fs::write(temp_dir.path().join("pages/b.ts"), "").unwrap();
        let file = create_test_file(
            temp_dir.path(),
            "main.ts",
            "new Worker(new URL('./worker.ts', import.meta.url), { type: 'module' });
new URL('logo.svg', import.meta.url);
new URL('https://example.com', import.meta.url);
new URL('.', import.meta.url);
const lazy = import.meta.glob('./pages/*.ts');
const eager = import.meta.glob(['./pages/a.ts'], { eager: true, import: 'setup' });",
        );
        let specs = parse_imports(&file).unwrap().specs;
        let got: Vec<(&str, SpecKind, u32)> =
            specs.iter().map(|s| (s.request.as_str(), s.kind, s.line)).collect();
        assert_eq!(
            got,
            [
                ("./worker.ts", SpecKind::Type, 1),
                ("./logo.svg", SpecKind::Type, 2),
                ("./pages/a.ts", SpecKind::Dynamic, 5),
                ("./pages/b.ts", SpecKind::Dynamic, 5),
                ("./pages/a.ts", SpecKind::Static, 6),
            ]
        );
        let uses = parse_file(&file, &Plugins::default(), true, false).unwrap().uses;
        let used = |r: &str, u: Used| uses.usages.contains(&Usage { request: r.into(), used: u });
        assert!(used("./pages/b.ts", Used::All));
        assert!(used("./pages/a.ts", Used::Name("setup".into())));
    }

    #[test]
    fn resolve_calls_find_files_without_loading_them() {
        let temp_dir = TempDir::new().unwrap();
        let file = create_test_file(
            temp_dir.path(),
            "test.js",
            "fork(require.resolve('./worker'));\nnew Worker(import.meta.resolve('./w2'));",
        );
        let specs = parse_imports(&file).unwrap().specs;
        let got: Vec<(&str, SpecKind)> =
            specs.iter().map(|s| (s.request.as_str(), s.kind)).collect();
        assert_eq!(got, [("./worker", SpecKind::Type), ("./w2", SpecKind::Type)]);
    }

    #[test]
    fn component_files_are_parsed_from_their_scripts() {
        let temp_dir = TempDir::new().unwrap();
        let vue = create_test_file(
            temp_dir.path(),
            "App.vue",
            "<template><div>{{ a }} é</div></template>\n<script setup lang=\"ts\">\nimport { a } from './a';\n</script>\n<script>export default {}</script>",
        );
        let astro = create_test_file(
            temp_dir.path(),
            "Page.astro",
            "---\nimport Layout from './Layout.astro';\n---\n<Layout><script>import './client';</script></Layout>",
        );
        let parsed = parse_imports(&vue).unwrap();
        assert_eq!(parsed.error, None);
        let got: Vec<(&str, u32)> =
            parsed.specs.iter().map(|s| (s.request.as_str(), s.line)).collect();
        assert_eq!(got, [("./a", 3)]);
        assert_eq!(parsed.exports.len(), 1);
        let got: Vec<String> =
            parse_imports(&astro).unwrap().specs.into_iter().map(|s| s.request).collect();
        assert_eq!(got, ["./Layout.astro", "./client"]);
    }

    #[test]
    fn test_require_in_expression() {
        let temp_dir = TempDir::new().unwrap();
        let cache = DashMap::new();
        let file = create_test_file(
            temp_dir.path(),
            "test.js",
            "const config = loadConfig(require('./config'));",
        );
        let imports = imports_for(&file, &cache).unwrap();
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].request, "./config");
    }

    #[test]
    fn test_type_only_import_skipped() {
        let temp_dir = TempDir::new().unwrap();
        let cache = DashMap::new();
        let file =
            create_test_file(temp_dir.path(), "test.ts", "import type { Foo } from './types';");
        let imports = imports_for(&file, &cache).unwrap();
        assert_eq!(imports.len(), 0);
    }

    #[test]
    fn type_only_imports_are_kept_as_type() {
        let temp_dir = TempDir::new().unwrap();
        let src = "import type { A } from './a';
import { type B } from './b';
import { type C, c } from './c';
export type { D } from './d';
export type * from './e';
import type F = require('./f');";
        let file = create_test_file(temp_dir.path(), "test.ts", src);
        let kinds: Vec<(String, SpecKind)> =
            parse_imports(&file).unwrap().specs.into_iter().map(|s| (s.request, s.kind)).collect();
        let t = |r: &str, k| (r.to_string(), k);
        assert_eq!(
            kinds,
            [
                t("./a", SpecKind::Type),
                t("./b", SpecKind::Type),
                t("./c", SpecKind::Static),
                t("./d", SpecKind::Type),
                t("./e", SpecKind::Type),
                t("./f", SpecKind::Type),
            ]
        );
    }

    #[test]
    fn html_scripts() {
        let temp_dir = TempDir::new().unwrap();
        create_test_file(temp_dir.path(), "package.json", "{}");
        fs::create_dir_all(temp_dir.path().join("pages")).unwrap();
        let src = r#"<!doctype html>
<script type="module" src="/src/main.ts"></script>
<script src="./legacy.js"></script>
<script src="https://cdn.example.com/x.js"></script>
<script type="importmap">{ "imports": { "a": "./a.js" } }</script>
<script type="module">
  import { start } from './inline';
</script>
<div data-src="/not-a-script.js"></div>"#;
        let file = create_test_file(&temp_dir.path().join("pages"), "index.html", src);
        let got: Vec<(String, u32)> =
            parse_imports(&file).unwrap().specs.into_iter().map(|s| (s.request, s.line)).collect();
        let t = |r: &str, line| (r.to_string(), line);
        assert_eq!(got, [t("../src/main.ts", 2), t("./legacy.js", 3), t("./inline", 7)]);
        assert!(Plugins::default().is_entry(&file));
        let plain = create_test_file(
            temp_dir.path(),
            "plain.html",
            "<p>hi</p><script src=\"https://x/y.js\"></script>",
        );
        assert!(!Plugins::default().is_entry(&plain));
    }

    #[test]
    fn mdx_imports_and_exports() {
        let temp_dir = TempDir::new().unwrap();
        let src = "import { Chart } from './chart';
import Intro from
  './intro.mdx';

# Title

Some text that says import nothing from 'prose'.

```js
import fake from 'in-a-fence';
```

export { meta } from './meta';

<Chart />
";
        let file = create_test_file(temp_dir.path(), "page.mdx", src);
        let parsed = parse_imports(&file).unwrap();
        assert_eq!(parsed.error, None);
        let got: Vec<(String, u32)> =
            parsed.specs.into_iter().map(|s| (s.request, s.line)).collect();
        let t = |r: &str, line| (r.to_string(), line);
        assert_eq!(got, [t("./chart", 1), t("./intro.mdx", 2), t("./meta", 13)]);
    }

    #[test]
    fn import_types_and_reference_directives_are_type_imports() {
        let temp_dir = TempDir::new().unwrap();
        let src = "/// <reference types=\"vite/client\" />
/// <reference path=\"./global.d.ts\" />
// <reference types=\"not-a-directive\" />
type A = typeof import('pkg-a');
type B = import('./b').B;";
        let file = create_test_file(temp_dir.path(), "test.ts", src);
        let got: Vec<(String, SpecKind, u32)> = parse_imports(&file)
            .unwrap()
            .specs
            .into_iter()
            .map(|s| (s.request, s.kind, s.line))
            .collect();
        let t = |r: &str, line| (r.to_string(), SpecKind::Type, line);
        assert_eq!(got, [t("pkg-a", 4), t("./b", 5), t("vite/client", 1)]);
    }

    #[test]
    fn declaration_files_import_only_types() {
        let temp_dir = TempDir::new().unwrap();
        let file = create_test_file(
            temp_dir.path(),
            "index.d.ts",
            "import { A } from './a';\nexport * from './b';\nexport declare function f(): A;",
        );
        let parsed = parse_imports(&file).unwrap();
        assert!(parsed.specs.iter().all(|s| s.kind == SpecKind::Type), "{:?}", parsed.specs);
        assert_eq!(parsed.specs.len(), 2);
        assert_eq!(parsed.exports.len(), 1);
    }

    #[test]
    fn test_mixed_type_and_runtime_import() {
        let temp_dir = TempDir::new().unwrap();
        let cache = DashMap::new();
        let file = create_test_file(
            temp_dir.path(),
            "test.ts",
            "import { type Foo, bar } from './utils';",
        );
        let imports = imports_for(&file, &cache).unwrap();
        // Should include because there's at least one runtime import (bar)
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].request, "./utils");
    }

    #[test]
    fn test_multiple_imports() {
        let temp_dir = TempDir::new().unwrap();
        let cache = DashMap::new();
        let file = create_test_file(
            temp_dir.path(),
            "test.js",
            "import foo from './foo';\nimport { bar } from './bar';\nimport './side-effect';",
        );
        let imports = imports_for(&file, &cache).unwrap();
        assert_eq!(imports.len(), 3);
        let requests: Vec<&str> = imports.iter().map(|s| s.request.as_str()).collect();
        assert!(requests.contains(&"./foo"));
        assert!(requests.contains(&"./bar"));
        assert!(requests.contains(&"./side-effect"));
    }

    #[test]
    fn test_cache_behavior() {
        let temp_dir = TempDir::new().unwrap();
        let cache = DashMap::new();
        let file = create_test_file(temp_dir.path(), "test.js", "import foo from './foo';");

        // First call should parse
        let imports1 = imports_for(&file, &cache).unwrap();
        assert_eq!(imports1.len(), 1);

        // Second call should use cache
        let imports2 = imports_for(&file, &cache).unwrap();
        assert_eq!(imports2.len(), 1);
        assert_eq!(imports1[0].request, imports2[0].request);

        // Cache should have entry
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn test_require_in_array() {
        let temp_dir = TempDir::new().unwrap();
        let cache = DashMap::new();
        let file = create_test_file(
            temp_dir.path(),
            "test.js",
            "const modules = [require('./a'), require('./b')];",
        );
        let imports = imports_for(&file, &cache).unwrap();
        assert_eq!(imports.len(), 2);
        let requests: Vec<&str> = imports.iter().map(|s| s.request.as_str()).collect();
        assert!(requests.contains(&"./a"));
        assert!(requests.contains(&"./b"));
    }

    #[test]
    fn test_require_in_object() {
        let temp_dir = TempDir::new().unwrap();
        let cache = DashMap::new();
        let file = create_test_file(
            temp_dir.path(),
            "test.js",
            "const config = { db: require('./db'), api: require('./api') };",
        );
        let imports = imports_for(&file, &cache).unwrap();
        assert_eq!(imports.len(), 2);
        let requests: Vec<&str> = imports.iter().map(|s| s.request.as_str()).collect();
        assert!(requests.contains(&"./db"));
        assert!(requests.contains(&"./api"));
    }

    #[test]
    fn test_require_in_conditional() {
        let temp_dir = TempDir::new().unwrap();
        let cache = DashMap::new();
        let file = create_test_file(
            temp_dir.path(),
            "test.js",
            "const mod = condition ? require('./a') : require('./b');",
        );
        let imports = imports_for(&file, &cache).unwrap();
        assert_eq!(imports.len(), 2);
        let requests: Vec<&str> = imports.iter().map(|s| s.request.as_str()).collect();
        assert!(requests.contains(&"./a"));
        assert!(requests.contains(&"./b"));
    }

    #[test]
    fn test_no_imports() {
        let temp_dir = TempDir::new().unwrap();
        let cache = DashMap::new();
        let file = create_test_file(temp_dir.path(), "test.js", "const x = 42;");
        let imports = imports_for(&file, &cache).unwrap();
        assert_eq!(imports.len(), 0);
    }

    #[test]
    fn test_typescript_file() {
        let temp_dir = TempDir::new().unwrap();
        let cache = DashMap::new();
        let file = create_test_file(
            temp_dir.path(),
            "test.ts",
            "import { Component } from './component';",
        );
        let imports = imports_for(&file, &cache).unwrap();
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].request, "./component");
    }

    #[test]
    fn test_jsx_file() {
        let temp_dir = TempDir::new().unwrap();
        let cache = DashMap::new();
        let file = create_test_file(temp_dir.path(), "test.jsx", "import React from 'react';");
        let imports = imports_for(&file, &cache).unwrap();
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].request, "react");
    }

    #[test]
    fn test_reexports_are_imports() {
        let temp_dir = TempDir::new().unwrap();
        let cache = DashMap::new();
        let file = create_test_file(
            temp_dir.path(),
            "index.ts",
            "export * from './a';\nexport * as b from './b';\nexport { c } from './c';\nexport type { T } from './types';\nexport { type U } from './types2';\nexport const local = 1;",
        );
        let imports = imports_for(&file, &cache).unwrap();
        let requests: Vec<&str> = imports.iter().map(|s| s.request.as_str()).collect();
        assert_eq!(requests, vec!["./a", "./b", "./c"]);
        assert!(imports.iter().all(|s| s.is_static()));
    }

    #[test]
    fn test_nested_require_and_dynamic_import() {
        let temp_dir = TempDir::new().unwrap();
        let cache = DashMap::new();
        let file = create_test_file(
            temp_dir.path(),
            "test.js",
            "function load() { if (x) { return require('./lazy'); } }\nasync function f() { const m = await import('./chunk'); }\nexport default require('./default');",
        );
        let imports = imports_for(&file, &cache).unwrap();
        let find = |r: &str| imports.iter().find(|s| s.request == r).unwrap().kind;
        assert_eq!(imports.len(), 3);
        assert_eq!(find("./lazy"), SpecKind::Static);
        assert_eq!(find("./chunk"), SpecKind::Dynamic);
        assert_eq!(find("./default"), SpecKind::Static);
    }

    #[test]
    fn test_ts_import_equals_require() {
        let temp_dir = TempDir::new().unwrap();
        let cache = DashMap::new();
        let file = create_test_file(
            temp_dir.path(),
            "test.ts",
            "import fs = require('fs-extra');\nimport type T = require('./types');",
        );
        let imports = imports_for(&file, &cache).unwrap();
        let requests: Vec<&str> = imports.iter().map(|s| s.request.as_str()).collect();
        assert_eq!(requests, vec!["fs-extra"]);
    }

    #[test]
    fn test_declare_module_blocks_are_ignored() {
        let temp_dir = TempDir::new().unwrap();
        let cache = DashMap::new();
        let file = create_test_file(
            temp_dir.path(),
            "test.ts",
            "declare module 'x' { import { Y } from 'y'; }\nimport './real';",
        );
        let imports = imports_for(&file, &cache).unwrap();
        let requests: Vec<&str> = imports.iter().map(|s| s.request.as_str()).collect();
        assert_eq!(requests, vec!["./real"]);
    }

    #[test]
    fn test_non_js_files_are_leaves() {
        let temp_dir = TempDir::new().unwrap();
        let cache = DashMap::new();
        let file = create_test_file(temp_dir.path(), "styles.css", "@import './other.css';");
        assert!(imports_for(&file, &cache).unwrap().is_empty());
    }

    #[test]
    fn test_parse_imports_reports_syntax_errors() {
        let temp_dir = TempDir::new().unwrap();
        let file = create_test_file(temp_dir.path(), "bad.ts", "import a from './a';\nconst = ;");
        let parsed = parse_imports(&file).unwrap();
        // oxc stops at the first fatal error, so the import list is lost and
        // the caller must be told rather than seeing a file with no imports.
        assert!(parsed.error.is_some());

        let ok = create_test_file(temp_dir.path(), "ok.js", "import a from './a';");
        assert!(parse_imports(&ok).unwrap().error.is_none());
    }
}
