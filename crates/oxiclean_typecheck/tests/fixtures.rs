//! Runs the checks on one small monorepo per package manager in
//! `tests/fixtures`: pnpm, npm, Bun and Deno.

use std::path::{Path, PathBuf};

use oxiclean_core::CommonArgs;
use oxiclean_typecheck::{
    CheckResult, Config, FindingKind, run_overlap_check, run_program_files_check,
    run_references_check,
};

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

fn program_files(name: &str) -> Vec<String> {
    let cfg = Config { common: CommonArgs::with_root(fixture(name)) };
    summary(&run_program_files_check(&cfg).expect("fixture analyses"))
}

fn overlap(name: &str) -> Vec<String> {
    let cfg = Config { common: CommonArgs::with_root(fixture(name)) };
    summary(&run_overlap_check(&cfg).expect("fixture analyses"))
}

/// pnpm: `tsconfig.build.json` inherits an `exclude` and so checks its
/// own `dist`, which `tsconfig.json` checks too; a root
/// `tsconfig.eslint.json` reaches into every package's `src`.
#[test]
fn pnpm() {
    assert_eq!(
        program_files("pnpm"),
        [
            "packages/app/tsconfig.build.json:2 build-output dist (1) fix: add \"dist\" to \"exclude\"",
            "packages/app/tsconfig.json:4 build-output dist (1) fix: add \"dist\" to \"exclude\"",
        ]
    );
    assert_eq!(
        overlap("pnpm"),
        [
            "tsconfig.eslint.json:3 shared-files packages/app (1)",
            "tsconfig.eslint.json:3 shared-files packages/ui (1)"
        ]
    );
}

/// npm workspaces: a root tsconfig that includes the packages, and a
/// minified file `allowJs` pulls in.
#[test]
fn npm() {
    assert_eq!(
        program_files("npm"),
        [
            "packages/lib/tsconfig.json:3 large-js allowJs (1) fix: add \"**/*.min.js\" to \"exclude\""
        ]
    );
    assert_eq!(overlap("npm"), ["tsconfig.json:3 shared-files packages/lib (1)"]);
}

/// Bun workspaces: a solution-style root, and a package whose explicit
/// `exclude` drops the default exclusion of its `outDir`.
#[test]
fn bun() {
    assert_eq!(
        program_files("bun"),
        [
            "packages/server/tsconfig.json:3 build-output build (1) fix: add \"build\" to \"exclude\""
        ]
    );
    assert!(overlap("bun").is_empty());
}

/// Deno workspaces have no tsconfig: Deno reads `compilerOptions` from
/// deno.json and type-checks the module graph, so there are no programs.
#[test]
fn deno() {
    assert!(program_files("deno").is_empty());
    assert!(overlap("deno").is_empty());
}

fn references(name: &str) -> Vec<String> {
    let cfg = Config { common: CommonArgs::with_root(fixture(name)) };
    summary(&run_references_check(&cfg).expect("fixture analyses"))
}

/// pnpm with project references: one of each kind of mismatch.
#[test]
fn references_pnpm() {
    assert_eq!(
        references("references-pnpm"),
        [
            "packages/app/tsconfig.json:5 stale-reference @acme/legacy (1) fix: remove { \"path\": \"../legacy\" } from \"references\"",
            "packages/app/tsconfig.json:4 missing-reference @acme/ui (1) fix: add { \"path\": \"../ui\" } to \"references\"",
            "packages/legacy/tsconfig.json:2 not-composite composite (1) fix: set \"compilerOptions.composite\": true",
            "packages/x/tsconfig.json:1 reference-cycle {x},{y} (2)",
            "tsconfig.json:7 missing-target ./packages/gone (1)",
        ]
        .map(|s| {
            let root = fixture("references-pnpm").canonicalize().unwrap();
            let p = |n: &str| root.join(format!("packages/{n}/tsconfig.json")).to_string_lossy().replace('\\', "/");
            s.replace("{x}", &p("x")).replace("{y}", &p("y"))
        })
    );
}

/// npm without references: a package.json `types` pointing at source two
/// packages import.
#[test]
fn references_npm() {
    assert_eq!(
        references("references-npm"),
        ["packages/core/tsconfig.json:0 source-imports core (2)"]
    );
}

/// Bun without references: a `paths` wildcard into each package's `src`.
#[test]
fn references_bun() {
    assert_eq!(
        references("references-bun"),
        ["packages/c/tsconfig.json:0 source-imports @acme/c (2)"]
    );
}

/// Deno has no tsconfig files, so no references.
#[test]
fn references_deno() {
    assert!(references("references-deno").is_empty());
}
