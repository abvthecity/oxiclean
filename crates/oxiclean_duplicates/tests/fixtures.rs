//! Runs the check on the projects in `tests/fixtures`. `knip-basic` and
//! `knip-alias` are knip's own fixtures; `forms` shows where oxiclean goes
//! beyond knip.

use std::path::{Path, PathBuf};

use oxiclean_core::CommonArgs;
use oxiclean_duplicates::{CheckResult, Config, FindingKind, run_duplicates_check};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

/// Each finding as `file: name:line, name:line, ...`.
fn duplicates(root: &Path) -> Vec<String> {
    let result: CheckResult = run_duplicates_check(&Config { common: CommonArgs::with_root(root) })
        .expect("fixture analyses");
    result
        .findings
        .iter()
        .map(|f| {
            let FindingKind::DuplicateExport(dup) = &f.kind else { panic!("{f:?}") };
            assert_eq!(f.value, dup.names.len());
            let file = f.file.strip_prefix(&result.root).unwrap().to_string_lossy();
            let names: Vec<String> =
                dup.names.iter().map(|n| format!("{}:{}", n.name, n.line)).collect();
            format!("{}: {}", file.replace('\\', "/"), names.join(", "))
        })
        .collect()
}

/// knip's `test/exports/basic.test.ts`: one duplicate, in my-module.ts.
#[test]
fn knip_basic_fixture() {
    assert_eq!(duplicates(&fixture("knip-basic")), ["my-module.ts: exportedResult:26, default:30"]);
}

/// knip's `test/exports/duplicate-exports-alias.test.ts`: `@alias` and
/// re-exports don't count, and neither does `export { X }; export default X`.
#[test]
fn knip_alias_fixture() {
    assert_eq!(duplicates(&fixture("knip-alias")), ["helpers.ts: isUntagged:6, isUntaggedAlias:8"]);
}

#[test]
fn originals_exported_later_and_forwarded_imports() {
    assert_eq!(
        duplicates(&fixture("forms")),
        ["src/alias-first.ts: long:3, short:2, default:4", "src/renames.ts: local:5, copy:6",]
    );
}

/// Declaration files are checked too, whether imported for types
/// (`types.d.ts`) or next to the JavaScript file they describe
/// (`useThing.d.ts`), as in MUI's `useAutocomplete.d.ts`.
#[test]
fn declaration_files() {
    assert_eq!(
        duplicates(&fixture("declarations")),
        ["src/types.d.ts: test:4, default:5", "src/useThing.d.ts: useThing:1, default:3"]
    );
}
