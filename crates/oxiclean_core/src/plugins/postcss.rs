//! PostCSS: the plugins and syntaxes its config files and package.json
//! `postcss` name by string. Tailwind's config imports what it uses, and
//! Tailwind 4's `@plugin` is read by the CSS plugin.

use std::path::Path;

use serde_json::Value;

use super::{
    Plugin,
    config::{self, is_path, names},
};

const FILES: &[&str] = &[
    "postcss.config.js",
    "postcss.config.cjs",
    "postcss.config.mjs",
    "postcss.config.ts",
    "postcss.config.mts",
    "postcss.config.cts",
    "postcss.config.json",
    ".postcssrc",
    ".postcssrc.json",
    ".postcssrc.js",
    ".postcssrc.cjs",
    ".postcssrc.mjs",
];

pub(super) struct Postcss;

impl Plugin for Postcss {
    fn name(&self) -> &'static str {
        "postcss"
    }

    /// The `plugins` of every config in `dir`, given as an object's keys
    /// (unless set to `false`) or as an array's strings, and its `parser`,
    /// `syntax` and `stringifier`.
    fn references(&self, dir: &Path) -> Vec<String> {
        let mut out = Vec::new();
        let configs = FILES
            .iter()
            .map(|n| dir.join(n))
            .filter(|p| p.is_file())
            .filter_map(|p| config::load(&p))
            .chain(config::package_field(dir, "postcss"));
        for c in configs {
            match c.get("plugins") {
                Some(Value::Object(plugins)) => out.extend(
                    plugins
                        .iter()
                        .filter(|(_, v)| **v != Value::Bool(false))
                        .map(|(k, _)| k.clone()),
                ),
                plugins => out.extend(names(plugins).into_iter().map(str::to_owned)),
            }
            for key in ["parser", "syntax", "stringifier"] {
                out.extend(names(c.get(key)).into_iter().map(str::to_owned));
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
    fn postcss_references() {
        let t = TempDir::new().unwrap();
        fs::write(
            t.path().join("postcss.config.mjs"),
            "export default { plugins: { '@tailwindcss/postcss': {}, autoprefixer: {}, cssnano: false } };",
        )
        .unwrap();
        fs::write(
            t.path().join(".postcssrc.json"),
            r#"{ "plugins": ["postcss-nested", "./local.js"], "syntax": "postcss-scss" }"#,
        )
        .unwrap();
        assert_eq!(
            Postcss.references(t.path()),
            ["@tailwindcss/postcss", "autoprefixer", "postcss-nested", "postcss-scss"]
        );
    }
}
