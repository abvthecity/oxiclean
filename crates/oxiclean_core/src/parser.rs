use anyhow::{Context, Result};
use dashmap::DashMap;
use log::{debug, trace, warn};
use oxc_allocator::Allocator;
use oxc_ast::ast::*;
use oxc_ast_visit::{Visit, walk};
use oxc_parser::{Parser as OxcParser, ParserReturn};
use oxc_span::SourceType;
use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::constants::JS_TS_EXTENSIONS;
use crate::types::{SpecKind, Specifier};

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
    cache.insert(file_buf, parsed.specs.clone());
    Ok(parsed.specs)
}

/// The imports of one file, plus the first syntax error if the file did not
/// parse cleanly. After a syntax error `specs` is incomplete, usually empty.
#[derive(Debug, Clone, Default)]
pub struct ParsedImports {
    pub specs: Vec<Specifier>,
    pub error: Option<String>,
}

/// Reads and parses `file`, returning every runtime import it makes.
///
/// Unlike [`imports_for`] this does not cache, and it reports syntax errors
/// instead of silently returning a partial list.
///
/// # Errors
/// Fails if the file can't be read.
pub fn parse_imports(file: &Path) -> Result<ParsedImports> {
    trace!("Parsing file for imports: {}", file.display());
    let src =
        fs::read_to_string(file).with_context(|| format!("Failed to read {}", file.display()))?;

    let st = source_type_for(file);
    let allocator = Allocator::default();
    let ParserReturn { program, errors, panicked, .. } =
        OxcParser::new(&allocator, &src, st).parse();

    let mut collector = ImportCollector::default();
    collector.visit_program(&program);
    let specs = collector.specs;

    let error = if panicked && errors.is_empty() {
        Some("parser gave up".to_string())
    } else {
        errors.first().map(|e| e.message.to_string())
    };
    debug!("Found {} import specifiers in {}", specs.len(), file.display());
    Ok(ParsedImports { specs, error })
}

/// Walks the whole AST collecting module requests, including re-exports and
/// `require()`/`import()` calls nested inside functions or conditionals.
#[derive(Default)]
struct ImportCollector {
    specs: Vec<Specifier>,
}

impl ImportCollector {
    fn push(&mut self, request: &str, kind: SpecKind) {
        trace!("Found {:?} import: '{}'", kind, request);
        self.specs.push(Specifier { request: request.to_string(), kind });
    }
}

impl<'a> Visit<'a> for ImportCollector {
    fn visit_import_declaration(&mut self, decl: &ImportDeclaration<'a>) {
        // Skip type-only imports (import type { Foo } from 'bar')
        if decl.import_kind.is_type() {
            return;
        }
        // Skip `import { type Foo } from 'bar'` when every specifier is a type.
        // No specifiers means a side-effect import: `import 'polyfill'`.
        let has_runtime_import = decl.specifiers.as_ref().is_none_or(|specifiers| {
            specifiers.is_empty()
                || specifiers.iter().any(|spec| match spec {
                    ImportDeclarationSpecifier::ImportSpecifier(s) => !s.import_kind.is_type(),
                    _ => true,
                })
        });
        if has_runtime_import {
            self.push(&decl.source.value, SpecKind::Static);
        }
    }

    fn visit_export_all_declaration(&mut self, decl: &ExportAllDeclaration<'a>) {
        if !decl.export_kind.is_type() {
            self.push(&decl.source.value, SpecKind::Static);
        }
    }

    fn visit_export_named_declaration(&mut self, decl: &ExportNamedDeclaration<'a>) {
        if let Some(source) = &decl.source {
            let has_runtime_export = !decl.export_kind.is_type()
                && (decl.specifiers.is_empty()
                    || decl.specifiers.iter().any(|s| !s.export_kind.is_type()));
            if has_runtime_export {
                self.push(&source.value, SpecKind::Static);
            }
        }
        walk::walk_export_named_declaration(self, decl);
    }

    fn visit_ts_import_equals_declaration(&mut self, decl: &TSImportEqualsDeclaration<'a>) {
        // import x = require('y')
        if !decl.import_kind.is_type()
            && let TSModuleReference::ExternalModuleReference(ext) = &decl.module_reference
        {
            self.push(&ext.expression.value, SpecKind::Static);
        }
    }

    fn visit_import_expression(&mut self, expr: &ImportExpression<'a>) {
        if let Expression::StringLiteral(sl) = &expr.source {
            self.push(&sl.value, SpecKind::Dynamic);
        } else if let Expression::TemplateLiteral(tl) = &expr.source
            && let Some(quasi) = tl.single_quasi()
        {
            self.push(&quasi, SpecKind::Dynamic);
        }
        walk::walk_import_expression(self, expr);
    }

    fn visit_call_expression(&mut self, call: &CallExpression<'a>) {
        if let Expression::Identifier(callee) = &call.callee
            && callee.name == "require"
            && call.arguments.len() == 1
            && let Some(Expression::StringLiteral(sl)) = call.arguments[0].as_expression()
        {
            self.push(&sl.value, SpecKind::Static);
        }
        walk::walk_call_expression(self, call);
    }

    // `declare module 'x' { import ... }` blocks describe types, not runtime edges
    fn visit_ts_module_declaration(&mut self, _decl: &TSModuleDeclaration<'a>) {}
}

fn source_type_for(path: &Path) -> SourceType {
    let ext = path.extension().and_then(|e| e.to_str());

    let mut st = SourceType::default()
        .with_jsx(matches!(ext, Some("tsx") | Some("jsx")))
        .with_typescript(matches!(ext, Some("ts") | Some("tsx") | Some("mts") | Some("cts")));

    // ESM heuristic - .mjs, .mts are ES modules
    if matches!(ext, Some("mjs") | Some("mts")) {
        st = st.with_module(true);
    }

    st
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
