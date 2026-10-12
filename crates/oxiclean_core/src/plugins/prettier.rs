//! Prettier: the shared config and plugins its config files and
//! package.json `prettier` name by string.

use std::path::Path;

use serde_json::Value;

use super::{
    Plugin,
    config::{self, is_path, names},
};

const FILES: &[&str] = &[
    ".prettierrc",
    ".prettierrc.json",
    ".prettierrc.json5",
    ".prettierrc.js",
    ".prettierrc.cjs",
    ".prettierrc.mjs",
    ".prettierrc.ts",
    ".prettierrc.mts",
    ".prettierrc.cts",
    "prettier.config.js",
    "prettier.config.cjs",
    "prettier.config.mjs",
    "prettier.config.ts",
    "prettier.config.mts",
    "prettier.config.cts",
];

pub(super) struct Prettier;

impl Plugin for Prettier {
    fn name(&self) -> &'static str {
        "prettier"
    }

    /// A config that is a string names a shared config; otherwise its
    /// `plugins`, and those of its `overrides`, are packages.
    fn references(&self, dir: &Path) -> Vec<String> {
        let mut out = Vec::new();
        let configs = FILES
            .iter()
            .map(|n| dir.join(n))
            .filter(|p| p.is_file())
            .filter_map(|p| config::load(&p))
            .chain(config::package_field(dir, "prettier"));
        for c in configs {
            if let Value::String(shared) = &c {
                out.push(shared.clone());
            }
            out.extend(names(c.get("plugins")).into_iter().map(str::to_owned));
            if let Some(Value::Array(overrides)) = c.get("overrides") {
                for o in overrides {
                    let plugins = o.get("options").and_then(|o| o.get("plugins"));
                    out.extend(names(plugins).into_iter().map(str::to_owned));
                }
            }
        }
        out.retain(|r| !is_path(r));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn prettier_references() {
        let t = TempDir::new().unwrap();
        fs::write(
            t.path().join(".prettierrc"),
            r#"{ "plugins": ["prettier-plugin-tailwindcss"],
                 "overrides": [{ "files": "*.svelte", "options": { "plugins": ["prettier-plugin-svelte"] } }] }"#,
        )
        .unwrap();
        fs::write(t.path().join("package.json"), r#"{ "prettier": "@acme/prettier-config" }"#)
            .unwrap();
        assert_eq!(
            Prettier.references(t.path()),
            ["prettier-plugin-tailwindcss", "prettier-plugin-svelte", "@acme/prettier-config"]
        );
    }
}
