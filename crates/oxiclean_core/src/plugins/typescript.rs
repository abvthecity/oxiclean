//! TypeScript: the packages tsconfig files name.

use std::{
    fs,
    path::{Path, PathBuf},
};

use serde_json::Value;

use super::Plugin;
use crate::jsonc::parse_jsonc;

pub(super) struct TypeScript;

impl Plugin for TypeScript {
    fn name(&self) -> &'static str {
        "typescript"
    }

    /// What the `tsconfig.json` and `tsconfig.*.json` files in `dir` name:
    /// `extends`, `compilerOptions.types` (each also as its `@types`
    /// package), `compilerOptions.jsxImportSource` and
    /// `compilerOptions.plugins[].name`.
    fn references(&self, dir: &Path) -> Vec<String> {
        let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };
        let mut files: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                    n == "tsconfig.json" || (n.starts_with("tsconfig.") && n.ends_with(".json"))
                })
            })
            .collect();
        files.sort();
        let mut out = Vec::new();
        for file in files {
            let Some(json) = fs::read_to_string(&file).ok().and_then(|s| parse_jsonc(&s)) else {
                continue;
            };
            match json.get("extends") {
                Some(Value::String(s)) => out.push(s.clone()),
                Some(Value::Array(a)) => {
                    out.extend(a.iter().filter_map(Value::as_str).map(str::to_owned));
                }
                _ => {}
            }
            let Some(options) = json.get("compilerOptions") else { continue };
            if let Some(Value::Array(types)) = options.get("types") {
                for t in types.iter().filter_map(Value::as_str) {
                    out.push(t.to_string());
                    out.push(types_package(t));
                }
            }
            if let Some(Value::String(s)) = options.get("jsxImportSource") {
                out.push(s.clone());
            }
            if let Some(Value::Array(plugins)) = options.get("plugins") {
                out.extend(
                    plugins.iter().filter_map(|p| p.get("name")?.as_str()).map(str::to_owned),
                );
            }
        }
        out
    }
}

/// The DefinitelyTyped package for a `types` entry: `@types/node` for
/// `node`, `@types/scope__pkg` for `@scope/pkg/sub`.
fn types_package(entry: &str) -> String {
    let (name, _) = crate::split_package_request(entry);
    match name.strip_prefix('@').and_then(|n| n.split_once('/')) {
        Some((scope, pkg)) => format!("@types/{scope}__{pkg}"),
        None => format!("@types/{name}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn tsconfig_references() {
        let t = TempDir::new().unwrap();
        fs::write(
            t.path().join("tsconfig.json"),
            r#"{
              // comments are allowed
              "extends": ["@tsconfig/node20/tsconfig.json", "./base.json"],
              "compilerOptions": {
                "types": ["vitest/globals", "@scope/x"],
                "jsxImportSource": "preact",
                "plugins": [{ "name": "typescript-plugin-css-modules" }],
              },
            }"#,
        )
        .unwrap();
        fs::write(t.path().join("tsconfig.build.json"), r#"{ "extends": "./tsconfig.json" }"#)
            .unwrap();
        assert_eq!(
            TypeScript.references(t.path()),
            [
                "./tsconfig.json",
                "@tsconfig/node20/tsconfig.json",
                "./base.json",
                "vitest/globals",
                "@types/vitest",
                "@scope/x",
                "@types/scope__x",
                "preact",
                "typescript-plugin-css-modules",
            ]
        );
    }
}
