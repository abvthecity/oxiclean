//! Runs both checks on the projects in `tests/fixtures`. The `knip-*`
//! fixtures are knip's own (see `tests/fixtures/README.md`) and give the same
//! results as knip; `forms` covers the export and import forms oxiclean reads.

use std::path::{Path, PathBuf};

use oxiclean_core::CommonArgs;
use oxiclean_unused_exports::{
    CheckResult, Config, FindingKind, run_unused_exports_check, run_unused_types_check,
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
            let FindingKind::UnusedExport(unused) = &f.kind else { panic!("{f:?}") };
            let file = f.file.strip_prefix(&result.root).unwrap().to_string_lossy();
            format!("{} {}:{}", file.replace('\\', "/"), unused.name, unused.line)
        })
        .collect()
}

fn exports(name: &str) -> Vec<String> {
    let cfg = Config { common: CommonArgs::with_root(fixture(name)) };
    lines(&run_unused_exports_check(&cfg).expect("fixture analyses"))
}

fn types(name: &str) -> Vec<String> {
    let cfg = Config { common: CommonArgs::with_root(fixture(name)) };
    lines(&run_unused_types_check(&cfg).expect("fixture analyses"))
}

/// knip's `test/exports/basic.test.ts`: 9 exports and 4 types.
#[test]
fn knip_exports_basic() {
    assert_eq!(
        exports("knip-exports-basic"),
        [
            "default.ts NamedExport:1",
            "dynamic-import.ts unusedZero:3",
            "my-mix.ts unusedInMix:1",
            "my-module.ts default:30",
            "my-module.ts unusedFunction:24",
            "my-module.ts unusedNumber:23",
            "my-namespace.ts nsUnusedKey:3",
            "named-exports.ts namedExport:7",
            "named-exports.ts renamedExport:6",
        ]
    );
    assert_eq!(
        types("knip-exports-basic"),
        [
            "my-module.ts MyAnyType:28",
            "my-namespace.ts MyNamespace:5",
            "types.ts MyEnum:3",
            "types.ts MyType:8",
        ]
    );
}

/// knip's `test/types/type-visibility.test.ts`: a type used in an exported
/// signature counts as used; 9 are not.
#[test]
fn knip_type_visibility() {
    assert!(exports("knip-type-visibility").is_empty());
    assert_eq!(
        types("knip-type-visibility"),
        [
            "src/lib.ts AppConfig:105",
            "src/lib.ts BaseEntity:64",
            "src/lib.ts CompletelyUnused:97",
            "src/lib.ts ErrorResult:57",
            "src/lib.ts FilterRule:82",
            "src/lib.ts InternalState:20",
            "src/lib.ts Metadata:73",
            "src/lib.ts PrivateParam:111",
            "src/lib.ts SuccessResult:53",
        ]
    );
}

/// knip's `test/imports/loaders.test.ts`: `() => import('./x')` passed to a
/// loader uses only the default export.
#[test]
fn knip_loaders() {
    assert_eq!(
        exports("knip-loaders"),
        [
            "pages/dashboard.ts unusedWidget:5",
            "pages/profile.ts unusedAvatar:5",
            "pages/settings.ts unusedToggle:7",
        ]
    );
    assert!(types("knip-loaders").is_empty());
}

/// knip's `test/types/declare-module-augmentation.test.ts`: types used inside
/// `declare module './x'` count as used.
#[test]
fn knip_module_augmentation() {
    assert!(exports("knip-module-augmentation").is_empty());
    assert!(types("knip-module-augmentation").is_empty());
}

/// knip's `test/re-exports/deep.test.ts`: uses travel through a chain of
/// `export *` and `export { x } from` files.
#[test]
fn knip_re_exports_deep() {
    assert!(exports("knip-re-exports-deep").is_empty());
    assert!(types("knip-re-exports-deep").is_empty());
}

#[test]
fn forms() {
    assert_eq!(
        exports("forms"),
        [
            "src/components.ts internalApi:19",
            "src/components.ts unused:12",
            "src/icons.ts Moon:2",
            "src/lazy.ts lazyUnused:2",
            "src/legacy.d.ts helper:2",
            "src/legacy.js helper:2",
            "src/re-exported.ts notReExported:2",
            "src/utils.ts parse:2",
        ]
    );
    assert_eq!(
        types("forms"),
        ["src/components.ts Color:20", "src/components.ts UnusedType:13", "src/events.ts Orphan:5",]
    );
}
