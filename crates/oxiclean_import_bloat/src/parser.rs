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

pub(crate) fn imports_for(
    file: &PathBuf,
    cache: &DashMap<PathBuf, Vec<Specifier>>,
) -> Result<Vec<Specifier>> {
    if let Some(v) = cache.get(file) {
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
    cache.insert(file.clone(), specs.clone());
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
