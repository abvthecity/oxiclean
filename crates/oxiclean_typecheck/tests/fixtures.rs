//! Runs the checks on one small monorepo per package manager in
//! `tests/fixtures`: pnpm, npm, Bun and Deno.

use std::path::{Path, PathBuf};

use oxiclean_core::CommonArgs;
use oxiclean_typecheck::{
    CheckResult, Config, FindingKind, run_ambient_types_check, run_isolated_declarations_check,
    run_large_literals_check, run_overlap_check, run_program_files_check, run_references_check,
    run_tsconfig_check, run_type_bloat_check,
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

fn options(name: &str) -> Vec<String> {
    let cfg = Config { common: CommonArgs::with_root(fixture(name)) };
    summary(&run_tsconfig_check(&cfg).expect("fixture analyses"))
}

fn ambient(name: &str) -> Vec<String> {
    let cfg = Config { common: CommonArgs::with_root(fixture(name)) };
    summary(&run_ambient_types_check(&cfg).expect("fixture analyses"))
}

/// pnpm: a shared base without `skipLibCheck` two packages extend, a
/// script running `tsc --noEmit`, a debug option, and a hoisted
/// `@types/jest` nothing declares (`@types/react` in the package's own
/// `node_modules` counts as declared).
#[test]
fn options_pnpm() {
    assert_eq!(
        options("options-pnpm"),
        [
            "packages/app/tsconfig.json:4 debug-option extendedDiagnostics (1) fix: set \"compilerOptions.extendedDiagnostics\": false",
            "packages/app/tsconfig.json:3 incremental incremental (1) fix: set \"compilerOptions.incremental\": true and \"tsBuildInfoFile\": \"./node_modules/.tmp/tsconfig.tsbuildinfo\"",
            "tsconfig.base.json:2 skip-lib-check skipLibCheck (2) fix: set \"compilerOptions.skipLibCheck\": true",
        ]
    );
    assert_eq!(
        ambient("options-pnpm"),
        ["packages/app/tsconfig.json:3 undeclared-types types (1)"]
    );
}

/// npm: `skipLibCheck` set, a script running `tsc` on an incremental
/// config, `traceResolution` on, TypeScript 6 (which loads no `@types`
/// without `types`), and a package extending a base that isn't installed.
#[test]
fn options_npm() {
    assert_eq!(
        options("options-npm"),
        [
            "packages/lib/tsconfig.json:5 debug-option traceResolution (1) fix: set \"compilerOptions.traceResolution\": false"
        ]
    );
    assert!(ambient("options-npm").is_empty());
}

/// Bun: a composite package `tsc -b` builds, so it isn't reported for
/// `incremental`, extending a shared base package without
/// `skipLibCheck`, and a hoisted `@types/express`.
#[test]
fn options_bun() {
    assert_eq!(
        options("options-bun"),
        [
            "packages/tsconfig/base.json:2 skip-lib-check skipLibCheck (1) fix: set \"compilerOptions.skipLibCheck\": true"
        ]
    );
    assert_eq!(
        ambient("options-bun"),
        ["packages/server/tsconfig.json:3 undeclared-types types (1)"]
    );
}

/// Deno: a member tsconfig with `skipLibCheck`, and `@types/node` from an
/// `npm:` import.
#[test]
fn options_deno() {
    assert!(options("options-deno").is_empty());
    assert!(ambient("options-deno").is_empty());
}

fn isolated(name: &str) -> Vec<String> {
    let cfg = Config { common: CommonArgs::with_root(fixture(name)) };
    summary(&run_isolated_declarations_check(&cfg).expect("fixture analyses"))
}

/// pnpm: a package whose exports all have types; ones an annotation away,
/// with `noEmit`, with `satisfies` in a file `files` reaches through a
/// re-export; and ones not reported: with `allowJs`, which `tsc` won't
/// combine with the option, and with a syntax error.
#[test]
fn isolated_pnpm() {
    assert_eq!(
        isolated("isolated-pnpm"),
        [
            "packages/app/tsconfig.json:1 few-annotations isolatedDeclarations (1)",
            "packages/near/tsconfig.json:2 few-annotations isolatedDeclarations (1)",
            "packages/ready/tsconfig.json:2 can-enable isolatedDeclarations (1) fix: set \"compilerOptions.isolatedDeclarations\": true",
            "packages/shape/tsconfig.json:1 few-annotations isolatedDeclarations (1)",
        ]
    );
}

/// npm: a library that has `isolatedDeclarations` on already, and one on
/// TypeScript 5.4, which doesn't know the option.
#[test]
fn isolated_npm() {
    assert!(isolated("isolated-npm").is_empty());
}

/// Bun: a package too many annotations away to report, and one ready.
#[test]
fn isolated_bun() {
    assert_eq!(
        isolated("isolated-bun"),
        [
            "packages/server/tsconfig.json:2 can-enable isolatedDeclarations (1) fix: set \"compilerOptions.isolatedDeclarations\": true"
        ]
    );
}

/// Deno: a JSR package whose entry re-exports a function without a return
/// type, which uses a private helper without one that isn't counted, and
/// a package without `exports`.
#[test]
fn isolated_deno() {
    assert_eq!(isolated("isolated-deno"), ["packages/add/deno.json:1 slow-types @scope/add (1)"]);
}

fn large(name: &str) -> Vec<String> {
    let cfg = Config { common: CommonArgs::with_root(fixture(name)) };
    summary(&run_large_literals_check(&cfg).expect("fixture analyses"))
}

/// pnpm, with the threshold at 20 values: a `.json` file two programs
/// import, an `as const` table, a table in a file the program reaches by
/// import only, a Supabase edge function (a Deno project inside a Node
/// repo), and ones not reported: a small literal, and a `.json` file a
/// `.d.json.ts` declares.
#[test]
fn large_pnpm() {
    assert_eq!(
        large("large-pnpm"),
        [
            "packages/app/data/table.ts:1 literal primes (22)",
            "packages/app/src/index.ts:7 literal icons (21)",
            "packages/app/src/strings.json:1 json strings.json (30)",
            "supabase/functions/hello/data.json:1 json data.json (24)",
        ]
    );
}

/// npm: an `allowJs` file of data, and ones not reported: `.json` files
/// programs with `resolveJsonModule` off (set off, and unset under
/// `node16`) import, and a declaration file.
#[test]
fn large_npm() {
    assert_eq!(large("large-npm"), ["packages/site/src/posts.js:1 literal posts (24)"]);
}

/// Bun: a dependency's `.json` file, imported with an attribute.
#[test]
fn large_bun() {
    assert_eq!(large("large-bun"), ["node_modules/emoji-data/data.json:1 json data.json (25)"]);
}

/// Deno, without a tsconfig: a `.json` import, a default export of data,
/// and one not reported: an array holding a variable.
#[test]
fn large_deno() {
    assert_eq!(
        large("large-deno"),
        [
            "packages/i18n/en.json:1 json en.json (22)",
            "packages/i18n/mod.ts:5 literal default (30)",
        ]
    );
}

fn type_bloat(name: &str) -> Vec<String> {
    let cfg = Config { common: CommonArgs::with_root(fixture(name)) };
    summary(&run_type_bloat_check(&cfg).expect("fixture analyses"))
}

/// pnpm, with the threshold at 500 lines: an SDK whose root re-exports
/// every client, imported from a root file and, as a type, from a file
/// reached by import; not reported: one client on its own, and
/// `@types/node`, which the SDK references but the program loads anyway.
/// The values match the lines of the files `tsc --listFiles` lists.
#[test]
fn type_bloat_pnpm() {
    assert_eq!(
        type_bloat("bloat-pnpm"),
        [
            "packages/app/src/helper.ts:2 heavy-types mega-sdk (615)",
            "packages/app/src/index.ts:1 heavy-types mega-sdk (615)",
        ]
    );
}

/// npm: a package typed only by its `@types` package, which pulls in more
/// with `/// <reference path>`; not reported: a small package and a
/// built-in module.
#[test]
fn type_bloat_npm() {
    assert_eq!(
        type_bloat("bloat-npm"),
        ["packages/web/src/index.ts:1 heavy-types util-belt (703)"]
    );
}

/// Bun: `exports` with a `types` condition; not reported: a wildcard
/// subpath of the same package.
#[test]
fn type_bloat_bun() {
    assert_eq!(type_bloat("bloat-bun"), ["packages/api/src/index.ts:1 heavy-types icon-kit (801)"]);
}

/// Deno, with a tsconfig for editors: a package whose `typesVersions`
/// maps its `types` entry.
#[test]
fn type_bloat_deno() {
    assert_eq!(
        type_bloat("bloat-deno"),
        ["packages/cli/src/main.ts:1 heavy-types cloud-apis (553)"]
    );
}
