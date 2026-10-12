//! Babel: the presets and plugins its config files and package.json
//! `babel` name by string.

use std::path::Path;

use serde_json::Value;

use super::{
    Plugin,
    config::{self, is_path, names},
};

const FILES: &[&str] = &[
    "babel.config.js",
    "babel.config.cjs",
    "babel.config.mjs",
    "babel.config.cts",
    "babel.config.json",
    ".babelrc",
    ".babelrc.js",
    ".babelrc.cjs",
    ".babelrc.mjs",
    ".babelrc.json",
];

pub(super) struct Babel;

impl Plugin for Babel {
    fn name(&self) -> &'static str {
        "babel"
    }

    /// The `presets` and `plugins` of every config in `dir`, as Babel
    /// resolves their names, and those of `env` and `overrides`.
    fn references(&self, dir: &Path) -> Vec<String> {
        let mut out = Vec::new();
        let configs = FILES
            .iter()
            .map(|n| dir.join(n))
            .filter(|p| p.is_file())
            .filter_map(|p| config::load(&p))
            .chain(config::package_field(dir, "babel"));
        for c in configs {
            read(&c, &mut out);
        }
        out
    }
}

fn read(config: &Value, out: &mut Vec<String>) {
    for (key, kind) in [("presets", "preset"), ("plugins", "plugin")] {
        out.extend(names(config.get(key)).into_iter().filter_map(|n| package(n, kind)));
    }
    if let Some(Value::Object(envs)) = config.get("env") {
        envs.values().for_each(|e| read(e, out));
    }
    if let Some(Value::Array(overrides)) = config.get("overrides") {
        overrides.iter().for_each(|o| read(o, out));
    }
}

/// The package Babel loads for preset or plugin `name`, as its
/// `standardizeName` does: `@babel/env` is `@babel/preset-env`, `env` is
/// `babel-preset-env`, `@scope` is `@scope/babel-preset`, `@scope/x` is
/// `@scope/babel-preset-x` and `module:x` is `x`. A name with a subpath,
/// such as `next/babel`, is used as is.
fn package(name: &str, kind: &str) -> Option<String> {
    if is_path(name) {
        return None;
    }
    if let Some(module) = name.strip_prefix("module:") {
        return Some(module.to_string());
    }
    let full = format!("babel-{kind}");
    let Some(scoped) = name.strip_prefix('@') else {
        let keep = name.contains('/') || name.starts_with(&format!("{full}-"));
        return Some(if keep { name.to_string() } else { format!("{full}-{name}") });
    };
    let Some((scope, rest)) = scoped.split_once('/') else {
        return Some(format!("{name}/{full}"));
    };
    Some(if rest.contains('/') || (scope == "babel" && rest.starts_with(&format!("{kind}-"))) {
        name.to_string()
    } else if scope == "babel" {
        format!("@babel/{kind}-{rest}")
    } else if rest.contains(&full) {
        name.to_string()
    } else {
        format!("@{scope}/{full}-{rest}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn babel_references() {
        let t = TempDir::new().unwrap();
        fs::write(
            t.path().join("babel.config.js"),
            r"module.exports = {
              presets: [['@babel/env', { targets: 'defaults' }], '@babel/preset-react', 'module:metro-react-native-babel-preset'],
              plugins: ['macros', './local-plugin', '@emotion'],
              env: { test: { plugins: ['istanbul'] } },
            };",
        )
        .unwrap();
        fs::write(t.path().join(".babelrc"), r#"{ "presets": ["next/babel"] }"#).unwrap();
        assert_eq!(
            Babel.references(t.path()),
            [
                "@babel/preset-env",
                "@babel/preset-react",
                "metro-react-native-babel-preset",
                "babel-plugin-macros",
                "@emotion/babel-plugin",
                "babel-plugin-istanbul",
                "next/babel",
            ]
        );
    }
}
