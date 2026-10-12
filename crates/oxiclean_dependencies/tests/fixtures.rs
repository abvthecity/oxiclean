//! Runs the checks on the projects in `tests/fixtures`. The `knip-*`
//! fixtures are knip's own (see `tests/fixtures/KNIP-LICENSE.md`) and give
//! the same results as knip; `workspace` covers workspaces, scripts and
//! tsconfig files.

use std::path::{Path, PathBuf};

use oxiclean_core::CommonArgs;
use oxiclean_dependencies::{
    CheckResult, Config, FindingKind, run_unused_dependencies_check,
    run_unused_dev_dependencies_check,
};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

/// Each finding as `file name:line`.
fn lines(result: &CheckResult) -> Vec<String> {
    result
        .findings
        .iter()
        .map(|f| {
            let FindingKind::UnusedDependency(dep) = &f.kind else { panic!("{f:?}") };
            let file = f.file.strip_prefix(&result.root).unwrap().to_string_lossy();
            format!("{} {}:{}", file.replace('\\', "/"), dep.name, dep.line)
        })
        .collect()
}

fn dependencies(name: &str) -> Vec<String> {
    let cfg = Config { common: CommonArgs::with_root(fixture(name)) };
    lines(&run_unused_dependencies_check(&cfg).expect("fixture analyses"))
}

fn dev_dependencies(name: &str) -> Vec<String> {
    let cfg = Config { common: CommonArgs::with_root(fixture(name)) };
    lines(&run_unused_dev_dependencies_check(&cfg).expect("fixture analyses"))
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
