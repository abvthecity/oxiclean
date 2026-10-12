//! Runs the check on the projects in `tests/fixtures`, each of which shows one
//! behaviour.

use std::path::{Path, PathBuf};

use oxiclean_core::CommonArgs;
use oxiclean_cycles::{CheckResult, Config, FindingKind, run_cycles_check};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

fn run(args: CommonArgs) -> CheckResult {
    run_cycles_check(&Config { common: args }).expect("fixture analyses")
}

/// Each cycle as `file:line -> request` steps relative to the root, plus the
/// size of its tangle.
fn cycles(result: &CheckResult) -> Vec<(Vec<String>, usize)> {
    result
        .findings
        .iter()
        .map(|f| {
            let FindingKind::Cycle(cycle) = &f.kind else { panic!("only cycles: {f:?}") };
            assert_eq!(f.value, cycle.steps.len());
            assert_eq!(f.file, cycle.steps[0].file);
            let steps = cycle
                .steps
                .iter()
                .map(|s| {
                    let rel = s.file.strip_prefix(&result.root).unwrap();
                    let rel = rel.to_string_lossy().replace('\\', "/");
                    format!("{rel}:{} -> {}", s.line, s.request)
                })
                .collect();
            (steps, cycle.group_size)
        })
        .collect()
}

#[test]
fn only_runtime_imports_make_cycles() {
    let result = run(CommonArgs::with_root(fixture("runtime")));
    // c <-> d is type-only on one side and lazy <-> lazy-target is dynamic.
    assert_eq!(
        cycles(&result),
        vec![(vec!["src/a.ts:2 -> ./b".to_string(), "src/b.ts:1 -> ./a".into()], 2)]
    );
    assert_eq!(result.error_count(), 1);
}

#[test]
fn dynamic_imports_count_when_included() {
    let root = fixture("runtime");
    let result = run(CommonArgs::with_root(&root).config(root.join("dynamic.oxicleanrc.json")));
    assert_eq!(
        cycles(&result),
        vec![
            (vec!["src/a.ts:2 -> ./b".to_string(), "src/b.ts:1 -> ./a".into()], 2),
            (
                vec![
                    "src/lazy-target.ts:1 -> ./lazy".to_string(),
                    "src/lazy.ts:2 -> ./lazy-target".into()
                ],
                2
            ),
        ]
    );
}

#[test]
fn a_cycle_is_reported_on_its_first_file_the_rule_applies_to() {
    // a.ts is ignored, so the cycle moves to b.ts instead of disappearing.
    let root = fixture("runtime");
    let result = run(CommonArgs::with_root(&root).config(root.join("ignore-a.oxicleanrc.json")));
    assert_eq!(
        cycles(&result),
        vec![(vec!["src/b.ts:1 -> ./a".to_string(), "src/a.ts:2 -> ./b".into()], 2)]
    );
}

#[test]
fn every_cycle_in_a_tangle_is_reported() {
    // a <-> b and b <-> c share b: one tangle of three files, two cycles.
    let result = run(CommonArgs::with_root(fixture("tangle")));
    assert_eq!(
        cycles(&result),
        vec![
            (vec!["src/a.js:1 -> ./b.js".to_string(), "src/b.js:2 -> ./a.js".into()], 3),
            (vec!["src/b.js:1 -> ./c.js".to_string(), "src/c.js:1 -> ./b.js".into()], 3),
        ]
    );
}

/// knip's own cycle fixture: the same four cycles as
/// `test/imports/circular-dependencies.test.ts` in knip, and a fifth through
/// a dynamic `import()` when those count.
#[test]
fn knip_circular_dependencies_fixture() {
    let root = fixture("knip");
    let files = |result: &CheckResult| -> Vec<String> {
        cycles(result)
            .into_iter()
            .map(|(steps, _)| {
                let files: Vec<&str> = steps.iter().map(|s| s.split(':').next().unwrap()).collect();
                files.join(" → ")
            })
            .collect()
    };
    let knip = [
        "apricot.ts → banana.ts",
        "citrus.ts → lemon.ts → lime.ts",
        "citrus.ts → lemon.ts → orange.ts",
        "ping.ts → pong.ts",
    ];

    let result = run(CommonArgs::with_root(&root));
    assert_eq!(files(&result), knip);
    // knip points both imports of ping.ts → pong.ts at line 1.
    assert_eq!(cycles(&result)[3].0, ["ping.ts:1 -> ./pong", "pong.ts:1 -> ./ping"]);

    let result = run(CommonArgs::with_root(&root).config(root.join("dynamic.oxicleanrc.json")));
    let mut with_dynamic = knip.to_vec();
    with_dynamic.push("zoodle.ts → zucchini.ts");
    assert_eq!(files(&result), with_dynamic);
}
