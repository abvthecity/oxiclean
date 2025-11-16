use anyhow::{Context, Result};
use dashmap::DashMap;
use log::{debug, trace};
use oxc_allocator::Allocator;
use oxc_ast::ast::*;
use oxc_parser::{Parser as OxcParser, ParserReturn};
use oxc_span::SourceType;
use std::{
    fs,
    path::{Path, PathBuf},
};

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
    trace!("Parsing file for imports: {}", file.display());
    let src =
        fs::read_to_string(file).with_context(|| format!("Failed to read {}", file.display()))?;

    let st = source_type_for(file);
    let allocator = Allocator::default();
    let ParserReturn { program, .. } = OxcParser::new(&allocator, &src, st).parse();

    let mut specs: Vec<Specifier> = Vec::new();

    for stmt in &program.body {
        match stmt {
            Statement::ImportDeclaration(decl) => {
                // Skip type-only imports (import type { Foo } from 'bar')
                if decl.import_kind.is_type() {
                    trace!("Skipping type-only import declaration in {}", file.display());
                    continue;
                }

                // Check if all specifiers are type-only (import { type Foo } from 'bar')
                // If there's at least one non-type import, we should include it
                let has_runtime_import = if let Some(specifiers) = &decl.specifiers {
                    specifiers.iter().any(|spec| match spec {
                        ImportDeclarationSpecifier::ImportSpecifier(s) => !s.import_kind.is_type(),
                        ImportDeclarationSpecifier::ImportDefaultSpecifier(_) => true,
                        ImportDeclarationSpecifier::ImportNamespaceSpecifier(_) => true,
                    })
                } else {
                    // No specifiers means something like: import 'side-effect'
                    true
                };

                if has_runtime_import {
                    let req = decl.source.value.to_string();
                    trace!("Found static import: '{}' in {}", req, file.display());
                    specs.push(Specifier { request: req, kind: SpecKind::Static });
                }
            }
            Statement::ExpressionStatement(es) => {
                // Recursively extract all require() and import() calls
                extract_require_from_expression(&es.expression, &mut specs);
            }
            Statement::VariableDeclaration(vd) => {
                // Handle const x = require('...') or const x = someFunc(require('...'))
                for decl in &vd.declarations {
                    if let Some(init) = &decl.init {
                        extract_require_from_expression(init, &mut specs);
                    }
                }
            }
            _ => {}
        }
    }

    debug!("Found {} import specifiers in {}", specs.len(), file.display());
    cache.insert(file_buf, specs.clone());
    Ok(specs)
}

fn extract_require_from_expression(expr: &Expression, specs: &mut Vec<Specifier>) {
    match expr {
        Expression::CallExpression(ce) => {
            // Check if this is a require() call
            if let Expression::Identifier(callee_ident) = &ce.callee
                && callee_ident.name.as_str() == "require"
                && !ce.arguments.is_empty()
                && let Some(Expression::StringLiteral(sl)) = ce.arguments[0].as_expression()
            {
                trace!("Found require() call: '{}'", sl.value);
                specs.push(Specifier { request: sl.value.to_string(), kind: SpecKind::Static });
            }
            // Recursively check arguments for nested require() calls
            for arg in &ce.arguments {
                if let Some(arg_expr) = arg.as_expression() {
                    extract_require_from_expression(arg_expr, specs);
                }
            }
            // Also check the callee in case it's a complex expression
            extract_require_from_expression(&ce.callee, specs);
        }
        Expression::ImportExpression(ie) => {
            if let Expression::StringLiteral(sl) = &ie.source {
                trace!("Found dynamic import(): '{}'", sl.value);
                specs.push(Specifier { request: sl.value.to_string(), kind: SpecKind::Dynamic });
            }
        }
        // Handle other expression types that might contain nested expressions
        Expression::ArrayExpression(ae) => {
            for elem in &ae.elements {
                if let Some(expr) = elem.as_expression() {
                    extract_require_from_expression(expr, specs);
                }
            }
        }
        Expression::ObjectExpression(oe) => {
            for prop in &oe.properties {
                if let Some(expr) = prop.as_property() {
                    extract_require_from_expression(&expr.value, specs);
                }
            }
        }
        Expression::ConditionalExpression(ce) => {
            extract_require_from_expression(&ce.test, specs);
            extract_require_from_expression(&ce.consequent, specs);
            extract_require_from_expression(&ce.alternate, specs);
        }
        Expression::AssignmentExpression(ae) => {
            extract_require_from_expression(&ae.right, specs);
        }
        Expression::ParenthesizedExpression(pe) => {
            extract_require_from_expression(&pe.expression, specs);
        }
        _ => {
            // For other expression types, we don't recurse further
        }
    }
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
}
