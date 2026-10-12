//! ESLint: the packages legacy configs (`.eslintrc*` and package.json
//! `eslintConfig`) name by string. Flat configs (`eslint.config.*`) import
//! what they use, and are entries already.

use std::path::Path;

use serde_json::Value;

use super::{
    Plugin,
    config::{self, is_path, names, prefixed},
};

/// Legacy config files, in ESLint's order.
const FILES: &[&str] =
    &[".eslintrc.js", ".eslintrc.cjs", ".eslintrc.json", ".eslintrc", ".eslintrc.mjs"];

pub(super) struct Eslint;

impl Plugin for Eslint {
    fn name(&self) -> &'static str {
        "eslint"
    }

    /// What every legacy config in `dir` names: `extends` (as
    /// `eslint-config-x`, or the plugin of `plugin:x/config`), `plugins`
    /// (as `eslint-plugin-x`), `parser`, `parserOptions.parser`, import
    /// resolvers in `settings`, and the same in each of `overrides`.
    fn references(&self, dir: &Path) -> Vec<String> {
        let mut out = Vec::new();
        let configs = FILES
            .iter()
            .map(|n| dir.join(n))
            .filter(|p| p.is_file())
            .filter_map(|p| config::load(&p))
            .chain(config::package_field(dir, "eslintConfig"));
        for c in configs {
            read(&c, &mut out);
        }
        out.retain(|r| !is_path(r));
        out
    }
}

fn read(config: &Value, out: &mut Vec<String>) {
    for name in names(config.get("extends")) {
        if let Some(plugin) = name.strip_prefix("plugin:") {
            // `plugin:react/recommended`, `plugin:@scope/x/recommended`.
            if let Some((plugin, _)) = plugin.rsplit_once('/') {
                out.push(plugin_package(plugin));
            }
        } else if !name.starts_with("eslint:") {
            out.push(if is_path(name) {
                name.to_string()
            } else {
                prefixed(name, "eslint-config")
            });
        }
    }
    out.extend(names(config.get("plugins")).into_iter().map(plugin_package));
    out.extend(names(config.get("parser")).into_iter().map(str::to_owned));
    out.extend(
        names(config.get("parserOptions").and_then(|o| o.get("parser")))
            .into_iter()
            .map(str::to_owned),
    );
    match config.get("settings").and_then(|s| s.get("import/resolver")) {
        Some(Value::Object(resolvers)) => {
            out.extend(resolvers.keys().map(|k| prefixed(k, "eslint-import-resolver")));
        }
        Some(Value::String(r)) => out.push(prefixed(r, "eslint-import-resolver")),
        _ => {}
    }
    if let Some(Value::Array(overrides)) = config.get("overrides") {
        for o in overrides {
            read(o, out);
        }
    }
}

/// The package of plugin `name`: `eslint-plugin-react` for `react`,
/// `@scope/eslint-plugin` for `@scope`.
fn plugin_package(name: &str) -> String {
    prefixed(name, "eslint-plugin")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn eslintrc_references() {
        let t = TempDir::new().unwrap();
        fs::write(
            t.path().join(".eslintrc.cjs"),
            r"module.exports = {
              extends: ['eslint:recommended', 'airbnb', 'plugin:@typescript-eslint/recommended',
                'plugin:react/recommended', './base.js', 'prettier'],
              parser: '@typescript-eslint/parser',
              plugins: ['import', '@acme'],
              settings: { 'import/resolver': { typescript: {} } },
              overrides: [{ files: ['*.vue'], parserOptions: { parser: 'vue-eslint-parser' } }],
            };",
        )
        .unwrap();
        fs::write(
            t.path().join("package.json"),
            r#"{ "eslintConfig": { "extends": "react-app" } }"#,
        )
        .unwrap();
        assert_eq!(
            Eslint.references(t.path()),
            [
                "eslint-config-airbnb",
                "@typescript-eslint/eslint-plugin",
                "eslint-plugin-react",
                "eslint-config-prettier",
                "eslint-plugin-import",
                "@acme/eslint-plugin",
                "@typescript-eslint/parser",
                "eslint-import-resolver-typescript",
                "vue-eslint-parser",
                "eslint-config-react-app",
            ]
        );
    }
}
