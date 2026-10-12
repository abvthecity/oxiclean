//! Expo: the config plugins `app.json` and `app.config.*` name, a
//! package.json `main` such as `expo-router/entry`, and Expo Router's
//! routes in `app`, which are entries.

use std::path::Path;

use serde_json::Value;

use super::{
    Plugin,
    config::{self, is_path, names},
};

const FILES: &[&str] = &[
    "app.json",
    "app.config.ts",
    "app.config.js",
    "app.config.mjs",
    "app.config.cjs",
    "app.config.json",
];

const EXTENSIONS: &[&str] = &["js", "jsx", "ts", "tsx"];

pub(super) struct Expo;

impl Plugin for Expo {
    fn name(&self) -> &'static str {
        "expo"
    }

    fn enablers(&self) -> &'static [&'static str] {
        &["expo"]
    }

    /// The `plugins` of the app config, under `expo` or at its top, and
    /// `main` when it names a package.
    fn references(&self, dir: &Path) -> Vec<String> {
        let mut out = Vec::new();
        for c in FILES
            .iter()
            .map(|n| dir.join(n))
            .filter(|p| p.is_file())
            .filter_map(|p| config::load(&p))
        {
            let app = c.get("expo").unwrap_or(&c);
            out.extend(names(app.get("plugins")).into_iter().map(str::to_owned));
        }
        if let Some(Value::String(main)) = config::package_field(dir, "main")
            && config::lists_package(dir, "expo")
        {
            out.push(main);
        }
        out.retain(|r| !is_path(r) && !r.is_empty());
        out
    }

    /// Every JS/TS file under `app` in a package that lists `expo-router`.
    fn is_entry(&self, file: &Path) -> bool {
        if !file.extension().and_then(|e| e.to_str()).is_some_and(|e| EXTENSIONS.contains(&e)) {
            return false;
        }
        file.ancestors().skip(1).any(|dir| {
            dir.file_name().is_some_and(|n| n == "app")
                && config::routes_owner(dir)
                    .is_some_and(|o| config::lists_package(o, "expo-router"))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn expo_config_and_routes() {
        let t = TempDir::new().unwrap();
        let dir = t.path();
        fs::write(
            dir.join("package.json"),
            r#"{ "main": "expo-router/entry", "dependencies": { "expo": "54", "expo-router": "6" } }"#,
        )
        .unwrap();
        fs::write(
            dir.join("app.json"),
            r#"{ "expo": { "plugins": ["expo-router", ["expo-camera", { "x": 1 }], "./plugins/local"] } }"#,
        )
        .unwrap();
        assert_eq!(Expo.references(dir), ["expo-router", "expo-camera", "expo-router/entry"]);
        assert!(Expo.is_entry(&dir.join("app/(tabs)/index.tsx")));
        assert!(!Expo.is_entry(&dir.join("components/x.tsx")));
    }
}
