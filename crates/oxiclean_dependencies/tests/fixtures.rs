//! Runs the checks on the projects in `tests/fixtures`. The `knip-*`
//! fixtures are knip's own (see `tests/fixtures/KNIP-LICENSE.md`) and give
//! the same results as knip; `workspace` covers workspaces, scripts and
//! tsconfig files.

use std::path::{Path, PathBuf};

use oxiclean_core::CommonArgs;
use oxiclean_dependencies::{
    CheckResult, Config, FindingKind, run_catalog_check, run_dependencies_check,
};

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

/// `plugins` turns a plugin off: with `html` off, a package only an HTML
/// page reaches is unused.
#[test]
fn plugins_option() {
    let t = tempfile::TempDir::new().unwrap();
    let write = |path: &str, text: &str| {
        let p = t.path().join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    };
    write("package.json", r#"{"name":"web","dependencies":{"preact":"1"}}"#);
    write("index.html", r#"<script type="module" src="/src/app.ts"></script>"#);
    write("src/app.ts", "import 'preact';");
    write("src/index.ts", "export {};");
    assert!(lines(&run(t.path()), "dependencies").is_empty());
    write(".oxicleanrc.json", r#"{ "plugins": { "html": false } }"#);
    assert_eq!(lines(&run(t.path()), "dependencies"), ["package.json preact:1"]);
}

/// Tool plugins: packages that ESLint, Babel, PostCSS and Prettier configs
/// name by string are used; `babel-plugin-unused`, which none names, is not.
#[test]
fn tool_configs() {
    assert_eq!(dev_dependencies("tool-configs"), ["package.json babel-plugin-unused:10"]);
}

/// Jest and Vitest plugins: what their configs name is used, and their
/// setup files are entries, so `@testing-library/jest-dom` and `msw`, which
/// only setup files import, are used; `jest-unused` is not.
#[test]
fn test_runners() {
    assert_eq!(dev_dependencies("test-runners"), ["package.json jest-unused:14"]);
}

/// Next.js plugin: App Router files and middleware are entries, so what
/// only they import is used.
#[test]
fn next_app() {
    assert_eq!(dependencies("next-app"), ["package.json unused-pkg:12"]);
}

/// Expo plugin: config plugins and `main` name packages, and Expo Router's
/// routes are entries.
#[test]
fn expo_app() {
    assert_eq!(dependencies("expo-app"), ["package.json unused-pkg:13"]);
}

/// Storybook, husky and lint-staged plugins: what `.storybook/main.ts`
/// names, what stories and `.storybook/preview.ts` import, and the binaries
/// husky's hooks and lint-staged run are used.
#[test]
fn repo_tooling() {
    assert_eq!(dev_dependencies("repo-tooling"), ["package.json unused-pkg:15"]);
}

/// Files `import.meta.glob` matches and workers `new URL(...,
/// import.meta.url)` names are reached, so what they import is used.
#[test]
fn vite_globs() {
    assert_eq!(dependencies("vite-globs"), ["package.json unused-pkg:8"]);
}

/// `catalog`: entries of pnpm's default and named catalogs that no
/// package.json or override refers to.
#[test]
fn catalog() {
    let cfg = Config { common: CommonArgs::with_root(fixture("catalog")) };
    let result = run_catalog_check(&cfg).expect("fixture analyses");
    let got: Vec<String> = result
        .findings
        .iter()
        .map(|f| {
            let FindingKind::UnusedCatalogEntry(e) = &f.kind else { panic!("{f:?}") };
            format!("{} {}:{}", e.catalog, e.name, e.line)
        })
        .collect();
    assert_eq!(got, ["default lodash:7", "legacy vue:13"]);
}
