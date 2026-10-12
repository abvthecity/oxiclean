//! Runs the check on the projects in `tests/fixtures`, each of which shows one
//! behaviour.

use std::path::{Path, PathBuf};

use oxiclean_core::CommonArgs;
use oxiclean_import_cycles::{CheckResult, Config, FindingKind, run_import_cycles_check};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

fn run(args: CommonArgs) -> CheckResult {
    run_import_cycles_check(&Config { common: args }).expect("fixture analyses")
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
