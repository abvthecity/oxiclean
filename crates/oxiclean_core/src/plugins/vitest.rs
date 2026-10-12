//! Vitest: what the `test` options of its config, or of Vite's, name.
//! Packages count as used, and setup files are entries.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{
    Plugin,
    config::{self, Named, names},
};

const FILES: &[&str] = &[
    "vitest.config.ts",
    "vitest.config.mts",
    "vitest.config.cts",
    "vitest.config.js",
    "vitest.config.mjs",
    "vitest.config.cjs",
    "vite.config.ts",
    "vite.config.mts",
    "vite.config.cts",
    "vite.config.js",
    "vite.config.mjs",
    "vite.config.cjs",
];

/// Reporters Vitest provides itself.
const BUILTIN_REPORTERS: &[&str] = &[
    "default",
    "basic",
    "verbose",
    "tree",
    "dot",
    "json",
    "junit",
    "tap",
    "tap-flat",
    "hanging-process",
    "github-actions",
    "blob",
];

pub(super) struct Vitest;

impl Plugin for Vitest {
    fn name(&self) -> &'static str {
        "vitest"
    }

    fn enablers(&self) -> &'static [&'static str] {
        &["vitest"]
    }

    fn references(&self, dir: &Path) -> Vec<String> {
        named(dir).packages
    }

    fn entries(&self, dir: &Path) -> Vec<PathBuf> {
        named(dir).files
    }
}

fn named(dir: &Path) -> Named {
    let mut out = Named::default();
    for c in
        FILES.iter().map(|n| dir.join(n)).filter(|p| p.is_file()).filter_map(|p| config::load(&p))
    {
        if let Some(test) = c.get("test") {
            read(dir, test, &mut out);
        }
    }
    out
}

/// `environment`, `setupFiles`, `globalSetup`, `reporters`, the coverage
/// and browser providers and `ui`, and the same in each of `projects`.
fn read(dir: &Path, test: &Value, out: &mut Named) {
    match test.get("environment").and_then(Value::as_str) {
        None | Some("node") => {}
        Some(env @ ("jsdom" | "happy-dom")) => out.add(dir, env),
        Some("edge-runtime") => out.add(dir, "@edge-runtime/vm"),
        Some(env) if env.starts_with("vitest-environment-") || env.contains('/') => {
            out.add(dir, env);
        }
        Some(env) => out.add(dir, &format!("vitest-environment-{env}")),
    }
    for key in ["setupFiles", "globalSetup"] {
        out.add_all(dir, test.get(key));
    }
    for reporter in names(test.get("reporters")) {
        match reporter {
            "html" => out.add(dir, "@vitest/ui"),
            r if BUILTIN_REPORTERS.contains(&r) => {}
            r => out.add(dir, r),
        }
    }
    match test.get("coverage").and_then(|c| c.get("provider")).and_then(Value::as_str) {
        Some("v8") | None if test.get("coverage").is_some_and(Value::is_object) => {
            out.add(dir, "@vitest/coverage-v8");
        }
        Some("istanbul") => out.add(dir, "@vitest/coverage-istanbul"),
        _ => {}
    }
    if let Some(browser) = test.get("browser")
        && browser.get("enabled") == Some(&Value::Bool(true))
    {
        out.add(dir, "@vitest/browser");
        if let Some(provider) = browser.get("provider").and_then(Value::as_str) {
            out.add(dir, provider);
            out.add(dir, &format!("@vitest/browser-{provider}"));
        }
    }
    if test.get("ui") == Some(&Value::Bool(true)) {
        out.add(dir, "@vitest/ui");
    }
    for key in ["projects", "workspace"] {
        if let Some(Value::Array(projects)) = test.get(key) {
            for p in projects.iter().filter_map(|p| p.get("test")) {
                read(dir, p, out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn vitest_config() {
        let t = TempDir::new().unwrap();
        fs::create_dir(t.path().join("test")).unwrap();
        fs::write(t.path().join("test/setup.ts"), "").unwrap();
        fs::write(
            t.path().join("vite.config.ts"),
            r"import { defineConfig } from 'vitest/config';
            export default defineConfig({
              plugins: [],
              test: {
                environment: 'happy-dom',
                setupFiles: ['./test/setup.ts'],
                reporters: ['default', 'html'],
                coverage: { reporter: ['text'] },
                projects: [{ test: { environment: 'edge-runtime' } }],
              },
            });",
        )
        .unwrap();
        let named = named(t.path());
        assert_eq!(
            named.packages,
            ["happy-dom", "@vitest/ui", "@vitest/coverage-v8", "@edge-runtime/vm"]
        );
        assert_eq!(named.files, [t.path().canonicalize().unwrap().join("test/setup.ts")]);
    }
}
