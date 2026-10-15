//! Runs `lint-config` on one small monorepo per package manager in
//! `tests/fixtures`: pnpm (ESLint 9 flat config), npm (ESLint 8 eslintrc
//! with oxlint), Bun (flat config and oxlint's type-aware mode) and Deno
//! (a TypeScript flat config).

use std::path::{Path, PathBuf};

use oxiclean_core::CommonArgs;
use oxiclean_lint::{CheckResult, Config, FindingKind, run_lint_config_check};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

/// Each finding as `file:line code key (value)`, with the fix's
/// description when there is one.
fn summary(result: &CheckResult) -> Vec<String> {
    result
        .findings
        .iter()
        .map(|f| {
            let FindingKind::Issue(issue) = &f.kind else { panic!("{f:?}") };
            let file =
                f.file.strip_prefix(&result.root).unwrap().to_string_lossy().replace('\\', "/");
            let fix =
                issue.fix.as_ref().map(|x| format!(" fix: {}", x.description)).unwrap_or_default();
            format!("{file}:{} {} {} ({}){fix}", issue.line, issue.code, issue.key, f.value)
        })
        .collect()
}

fn lint_config(name: &str) -> Vec<String> {
    let cfg = Config { common: CommonArgs::with_root(fixture(name)) };
    summary(&run_lint_config_check(&cfg).expect("fixture analyses"))
}

/// pnpm, ESLint 9: `project` globs over two tsconfigs, set for every file
/// but the config itself (so for a build script too); `tsconfigRootDir`
/// is left out, which typescript-eslint 8.37+ infers as the config reads
/// `tseslint.configs`; `import-x/no-cycle`, which a test-only override
/// doesn't turn off; oxlint in the scripts without eslint-plugin-oxlint;
/// and two tsconfig `outDir`s the config doesn't ignore.
#[test]
fn pnpm() {
    assert_eq!(
        lint_config("lint-pnpm"),
        [
            "eslint.config.mjs:1 build-output packages/api/dist,packages/web/build (2) fix: add a global ignores entry for \"packages/api/dist/**\", \"packages/web/build/**\"",
            "eslint.config.mjs:10 project-globs project (2)",
            "eslint.config.mjs:1 eslint-plugin-oxlint eslint-plugin-oxlint (1)",
            "eslint.config.mjs:14 slow-rule import-x/no-cycle (1)",
            "eslint.config.mjs:10 typed-js-files parserOptions (1)",
        ]
    );
}

/// npm, ESLint 8 with oxlint: type information for a `.js` file, two slow
/// import rules, and a committed `build` directory (a tsconfig's
/// `outDir`) that neither linter ignores. oxlint reads `.gitignore`, so
/// only output git tracks is reported for it.
#[test]
fn npm() {
    assert_eq!(
        lint_config("lint-npm"),
        [
            ".eslintrc.json:2 eslint-plugin-oxlint eslint-plugin-oxlint (1)",
            ".eslintrc.json:10 slow-rule import/namespace (1)",
            ".eslintrc.json:9 slow-rule import/no-unused-modules (1)",
            ".eslintrc.json:7 build-output packages/lib/build (1) fix: add \"packages/lib/build/**\" to \"ignorePatterns\"",
            ".eslintrc.json:5 typed-js-files parserOptions (1)",
            ".oxlintrc.json:4 build-output packages/lib/build (1) fix: add \"packages/lib/build/**\" to \"ignorePatterns\"",
        ]
    );
}

/// Bun: oxlint's type-aware mode on, in the config and a script, with the
/// categories that hold type-aware rules off; `project: true` on
/// typescript-eslint 8; and a `.next` directory git ignores, which oxlint
/// skips but ESLint doesn't. eslint-plugin-oxlint is used, and `dist` is
/// ignored already.
#[test]
fn bun() {
    assert_eq!(
        lint_config("lint-bun"),
        [
            ".oxlintrc.json:4 type-aware-unused typeAware (1) fix: set \"options.typeAware\": false",
            "eslint.config.js:5 build-output packages/app/.next (1) fix: add \"packages/app/.next/**\" to the global ignores",
            "eslint.config.js:10 project-service project (1)",
            "package.json:6 type-aware-unused lint (1)",
        ]
    );
}

/// Deno, an `eslint.config.ts`: `projectService` for every file, except
/// that `scripts/` depends on an environment variable, so only
/// `tools/release.mjs` is certain; `import/no-deprecated`; and a `dist`
/// git ignores.
#[test]
fn deno() {
    assert_eq!(
        lint_config("lint-deno"),
        [
            "eslint.config.ts:20 slow-rule import/no-deprecated (1)",
            "eslint.config.ts:1 build-output packages/api/dist (1) fix: add a global ignores entry for \"packages/api/dist/**\"",
            "eslint.config.ts:11 typed-js-files parserOptions (1)",
        ]
    );
}
