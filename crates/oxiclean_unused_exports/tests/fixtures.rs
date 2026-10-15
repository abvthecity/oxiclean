//! Runs the checks on the projects in `tests/fixtures`. The `knip-*`
//! fixtures are knip's own (see `tests/fixtures/KNIP-LICENSE.md`) and give
//! the same results as knip; `forms` covers the export and import forms
//! oxiclean reads.

use std::path::{Path, PathBuf};

use oxiclean_core::CommonArgs;
use oxiclean_unused_exports::{
    CheckResult, Config, FindingKind, Unused, run_unused_command, run_unused_enum_members_check,
    run_unused_exports_check, run_unused_namespace_members_check, run_unused_types_check,
};

/// The `unused` command shares one graph between its rules, built to read
/// members when any member rule is on; each rule must find what it finds
/// on its own.
#[test]
fn the_unused_command_matches_each_rule() {
    for entry in std::fs::read_dir(fixture("")).unwrap() {
        let path = entry.unwrap().path();
        if !path.is_dir() {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let cfg = Config { common: CommonArgs::with_root(&path) };
        let command = run_unused_command(&cfg).expect("fixture analyses");
        let rules: Vec<&str> = command.rules.iter().map(|r| r.rule).collect();
        assert_eq!(rules, Unused::ALL.map(Unused::rule), "{name}");
        let alone = [exports(&name), types(&name), enum_members(&name), namespace_members(&name)];
        for (r, alone) in command.rules.iter().zip(alone) {
            assert_eq!(lines(&r.result), alone, "{name}: {}", r.rule);
        }
    }
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

/// Each finding as `file name:line`.
fn lines(result: &CheckResult) -> Vec<String> {
    result
        .findings
        .iter()
        .map(|f| {
            let (FindingKind::UnusedExport(unused) | FindingKind::UnusedMember(unused)) = &f.kind
            else {
                panic!("{f:?}")
            };
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

fn enum_members(name: &str) -> Vec<String> {
    let cfg = Config { common: CommonArgs::with_root(fixture(name)) };
    lines(&run_unused_enum_members_check(&cfg).expect("fixture analyses"))
}

fn namespace_members(name: &str) -> Vec<String> {
    let cfg = Config { common: CommonArgs::with_root(fixture(name)) };
    lines(&run_unused_namespace_members_check(&cfg).expect("fixture analyses"))
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

/// knip's `test/types/enum-members.test.ts`: members used in a type or in
/// their own file count; the entry's enum and enums used only as a type are
/// not checked.
#[test]
fn knip_enum_members() {
    assert_eq!(
        enum_members("knip-enum-members"),
        ["members.ts MyEnum.B_Unused:9", "members.ts MyEnum.D-Key:11"]
    );
}

/// knip's `test/types/enum-members-element-access*.test.ts` and
/// `enum-members-enumerated.test.ts`: `E[key]`, a numeric key, an alias and
/// `Object.keys(E)` use every member, through renames and re-exports too;
/// a string key uses that member.
#[test]
fn knip_enum_members_read_as_a_whole() {
    for name in [
        "knip-enum-members-element-access",
        "knip-enum-members-element-access-resolution",
        "knip-enum-members-enumerated",
    ] {
        assert!(enum_members(name).is_empty(), "{name}");
    }
    assert_eq!(
        enum_members("knip-enum-members-element-access-string-key"),
        [
            "codes.ts NamedKey.unusedFirst:3",
            "codes.ts NamedKey.unusedSecond:4",
            "codes.ts NamespaceNamedKey.unused:15",
            "codes.ts NumberLikeKey.unused:22",
        ]
    );
}

/// knip's `test/namespaces/ts-namespace.test.ts`: 13 namespace members and
/// 2 enum members. Parameters that shadow a namespace don't use it, and the
/// members of a namespace merged with an enum are enum members.
#[test]
fn knip_ts_namespace() {
    assert_eq!(
        namespace_members("knip-ts-namespace"),
        [
            "members.ts Animals.unusedDog:31",
            "members.ts Fruits.Tropical.unusedPapaya:25",
            "members.ts Fruits.unusedBanana:21",
            "members.ts Overloads.untagged:80",
            "members.ts Seasons.unusedCount:60",
            "members.ts Shapes.unusedSquare:40",
            "members.ts Standalone.unusedValue:50",
            "merged.ts Validator.unusedMinLength:8",
            "merged.ts format.unusedPadding:16",
            "modules.ts Colors.Shades.unusedLight:8",
            "modules.ts Colors.unusedBlue:4",
            "types.ts Types.UnusedInterface:7",
            "types.ts Types.UnusedType:3",
        ]
    );
    assert_eq!(
        enum_members("knip-ts-namespace"),
        ["merged.ts Status.Inactive:21", "merged.ts Status.unusedDefault:27"]
    );
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
        [
            "src/components.ts Color:20",
            "src/components.ts UnusedType:13",
            "src/events.ts Orphan:5",
            "src/members.ts Untouched:21",
        ]
    );
    // Uses through a re-export count; the entry re-exports `Mode`, so all of
    // it is used, and `Untouched` is an unused type, not unused members.
    assert_eq!(
        enum_members("forms"),
        ["src/members.ts Level.High:3", "src/members.ts Level.Unused:6"]
    );
    assert_eq!(namespace_members("forms"), ["src/members.ts Api.put:17"]);
}

/// Members read through a re-export count whatever order the files are
/// read in, and an export named with a dot is still one name.
#[test]
fn member_reads() {
    assert_eq!(exports("member-reads"), ["b.ts sum:2", "dotted.ts a:3"]);
    assert_eq!(enum_members("member-reads"), ["enum.ts E.C:1"]);
}
