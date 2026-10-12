//! Runs the checks on the projects in `tests/fixtures`. The `knip-*`
//! fixtures are knip's own (see `tests/fixtures/KNIP-LICENSE.md`) and give
//! the same results as knip; `workspace` covers workspaces, scripts and
//! tsconfig files.

use std::path::{Path, PathBuf};

use oxiclean_core::CommonArgs;
use oxiclean_dependencies::{CheckResult, Config, FindingKind, run_dependencies_check};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

/// Each finding in package.json field `field` as `file name:line`.
fn lines(result: &CheckResult, field: &str) -> Vec<String> {
    result
        .findings
        .iter()
        .filter_map(|f| {
            let FindingKind::UnusedDependency(dep) = &f.kind else { panic!("{f:?}") };
            let file = f.file.strip_prefix(&result.root).unwrap().to_string_lossy();
            (dep.field == field)
                .then(|| format!("{} {}:{}", file.replace('\\', "/"), dep.name, dep.line))
        })
        .collect()
}

fn run(root: &Path) -> CheckResult {
    let cfg = Config { common: CommonArgs::with_root(root) };
    run_dependencies_check(&cfg).expect("fixture analyses")
}

fn dependencies(name: &str) -> Vec<String> {
    lines(&run(&fixture(name)), "dependencies")
}

fn dev_dependencies(name: &str) -> Vec<String> {
    lines(&run(&fixture(name)), "devDependencies")
}

/// knip's `test/dependencies/basic.test.ts`: two dependencies and one
/// devDependency. `.oxicleanrc.json` sets the entry and ignored package
/// that knip's `knip.json` sets.
#[test]
fn knip_basic() {
    assert_eq!(
        dependencies("knip-basic"),
        ["package.json @tootallnate/once:8", "package.json fs-extra:10"]
    );
    assert_eq!(dev_dependencies("knip-basic"), ["package.json mocha:23"]);
}

/// knip's `test/dependencies/definitely-typed.test.ts`: `@types/mocha` is
/// unused although a script runs mocha, and `@types/react-dom` is used as
/// `react-dom` is a peer dependency of the imported `next`.
#[test]
fn knip_definitely_typed() {
    assert!(dependencies("knip-definitely-typed").is_empty());
    assert_eq!(
        dev_dependencies("knip-definitely-typed"),
        ["package.json @types/mocha:14", "package.json @types/unused:17"]
    );
}

/// knip's `test/dependencies/types.test.ts`: `@types` packages of packages
/// that ship their own types are unused.
#[test]
fn knip_types() {
    assert!(dependencies("knip-types").is_empty());
    assert_eq!(
        dev_dependencies("knip-types"),
        ["package.json @types/ajv:8", "package.json @types/eslint__js:9"]
    );
}

/// knip's `test/dependencies/duplicate-dependencies.test.ts`.
#[test]
fn knip_duplicate_dependencies() {
    assert!(dependencies("knip-duplicate-dependencies").is_empty());
    assert_eq!(dev_dependencies("knip-duplicate-dependencies"), ["package.json typescript:7"]);
}

#[test]
fn workspace() {
    assert_eq!(
        dependencies("workspace"),
        ["packages/app/package.json lodash:11", "packages/app/package.json only-in-unreachable:12",]
    );
    assert_eq!(
        dev_dependencies("workspace"),
        [
            "package.json prettier:15",
            "packages/app/package.json @types/lodash:16",
            "packages/app/package.json react:21",
            "packages/ui/package.json left-pad:8",
        ]
    );
}

/// The `fields` option picks the fields to check, per package too.
#[test]
fn fields_option() {
    let t = tempfile::TempDir::new().unwrap();
    let write = |path: &str, text: &str| {
        let p = t.path().join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    };
    write("pnpm-workspace.yaml", "packages:\n  - 'packages/*'\n");
    write(
        "package.json",
        r#"{"name":"root","dependencies":{"a":"1"},"devDependencies":{"b":"1"}}"#,
    );
    write(
        "packages/app/package.json",
        r#"{"name":"app","main":"index.ts","dependencies":{"c":"1"},"devDependencies":{"d":"1"}}"#,
    );
    write("packages/app/index.ts", "export {};");
    write(
        ".oxicleanrc.json",
        r#"{
          "rules": { "dependencies": ["error", { "fields": ["dependencies"] }] },
          "overrides": [
            { "packages": ["app"], "rules": { "dependencies": ["error", { "fields": ["devDependencies"] }] } }
          ]
        }"#,
    );
    let result = run(t.path());
    assert_eq!(lines(&result, "dependencies"), ["package.json a:1"]);
    assert_eq!(lines(&result, "devDependencies"), ["packages/app/package.json d:1"]);
}
