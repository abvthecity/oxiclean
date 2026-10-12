//! Jest: what its config names. Packages count as used, and setup files
//! are entries.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{
    Plugin,
    config::{self, Named, names},
};

const FILES: &[&str] = &[
    "jest.config.js",
    "jest.config.ts",
    "jest.config.mjs",
    "jest.config.cjs",
    "jest.config.mts",
    "jest.config.cts",
    "jest.config.json",
];

/// Reporters Jest provides itself.
const BUILTIN_REPORTERS: &[&str] = &["default", "summary", "github-actions"];

pub(super) struct Jest;

impl Plugin for Jest {
    fn name(&self) -> &'static str {
        "jest"
    }

    fn enablers(&self) -> &'static [&'static str] {
        &["jest"]
    }

    fn references(&self, dir: &Path) -> Vec<String> {
        named(dir).packages
    }

    fn entries(&self, dir: &Path) -> Vec<PathBuf> {
        named(dir).files
    }
}

/// What the Jest configs in `dir` and its package.json `jest` name.
fn named(dir: &Path) -> Named {
    let mut out = Named::default();
    let configs = FILES
        .iter()
        .map(|n| dir.join(n))
        .filter(|p| p.is_file())
        .filter_map(|p| config::load(&p))
        .chain(config::package_field(dir, "jest"));
    for c in configs {
        read(dir, &c, &mut out);
    }
    out
}

/// `preset`, `testEnvironment` (`jsdom` as `jest-environment-jsdom`),
/// `runner`, `testRunner`, `transform`, setup files, `reporters`,
/// `snapshotSerializers`, `watchPlugins`, `moduleNameMapper` targets and
/// the like, and the same in each of `projects`.
fn read(dir: &Path, c: &Value, out: &mut Named) {
    for key in [
        "preset",
        "testRunner",
        "setupFiles",
        "setupFilesAfterEnv",
        "globalSetup",
        "globalTeardown",
        "snapshotSerializers",
        "watchPlugins",
        "testResultsProcessor",
        "resolver",
        "testSequencer",
        "dependencyExtractor",
    ] {
        out.add_all(dir, c.get(key));
    }
    if let Some(env) = c.get("testEnvironment").and_then(Value::as_str)
        && env != "node"
    {
        out.add(dir, &prefixed_simple(env, "jest-environment"));
    }
    if let Some(runner) = c.get("runner").and_then(Value::as_str) {
        out.add(dir, &prefixed_simple(runner, "jest-runner"));
    }
    for reporter in names(c.get("reporters")) {
        if !BUILTIN_REPORTERS.contains(&reporter) {
            out.add(dir, reporter);
        }
    }
    for key in ["transform", "moduleNameMapper"] {
        if let Some(Value::Object(map)) = c.get(key) {
            for v in map.values() {
                out.add_all(dir, Some(v));
            }
        }
    }
    if let Some(Value::Array(projects)) = c.get("projects") {
        for p in projects.iter().filter(|p| p.is_object()) {
            read(dir, p, out);
        }
    }
}

/// `prefix-name` for a plain `name`, as Jest tries first; a scoped,
/// subpath, already prefixed or path name is used as is.
fn prefixed_simple(name: &str, prefix: &str) -> String {
    if name.starts_with(&format!("{prefix}-")) || name.contains('/') || name.starts_with('@') {
        name.to_string()
    } else {
        format!("{prefix}-{name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn jest_config() {
        let t = TempDir::new().unwrap();
        fs::write(t.path().join("jest.setup.ts"), "").unwrap();
        fs::write(
            t.path().join("jest.config.js"),
            r"module.exports = {
              preset: 'ts-jest',
              testEnvironment: 'jsdom',
              setupFilesAfterEnv: ['<rootDir>/jest.setup', '@testing-library/jest-dom'],
              transform: { '^.+\\.svg$': 'jest-svg-transformer', '^.+\\.tsx?$': ['@swc/jest', {}] },
              moduleNameMapper: { '\\.css$': 'identity-obj-proxy', '^@/(.*)$': '<rootDir>/src/$1' },
              reporters: ['default', 'jest-junit'],
            };",
        )
        .unwrap();
        let named = named(t.path());
        assert_eq!(
            named.packages,
            [
                "ts-jest",
                "@testing-library/jest-dom",
                "jest-environment-jsdom",
                "jest-junit",
                "jest-svg-transformer",
                "@swc/jest",
                "identity-obj-proxy",
            ]
        );
        assert_eq!(named.files, [t.path().canonicalize().unwrap().join("jest.setup.ts")]);
    }
}
