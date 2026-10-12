//! Barrel files: modules that mostly re-export others, scored by how many
//! modules importing them pulls in.

use std::collections::HashSet;

use oxiclean_core::{
    Analysis, Barrel, BuildOptions, CheckResult, Finding, FindingKind, Module, Result, RulePolicy,
};
use tracing::info;

use crate::config::{BARRELS_DEFAULT_THRESHOLD, BARRELS_RULE, Config};

/// Runs the barrel files check described by `cfg`.
///
/// # Errors
/// Fails if the project root can't be found, an entry glob is invalid, or no
/// entry files are found.
pub fn run_barrels_check(cfg: &Config) -> Result<CheckResult> {
    info!("Starting {BARRELS_RULE} check");
    let analysis = cfg.common.analyze_with(BuildOptions::default().read_exports(true))?;
    let policy = analysis.policy(BARRELS_RULE, BARRELS_DEFAULT_THRESHOLD)?;
    Ok(check_barrels(&analysis, &policy))
}

/// Flags each barrel file whose import pulls in at least its file's
/// threshold of unique modules, itself included. Only runtime imports
/// count, so type-only re-exports pull in nothing.
#[must_use]
pub fn check_barrels(analysis: &Analysis, policy: &RulePolicy) -> CheckResult {
    if policy.min_threshold().is_none() {
        info!("{BARRELS_RULE} is disabled");
        return analysis.finish(policy, Vec::new());
    }
    let graph = &analysis.graph;
    let mut importers = vec![0usize; graph.len()];
    for (_, module) in graph.iter() {
        let targets: HashSet<_> = module.edges.iter().map(|e| e.target).collect();
        for t in targets {
            importers[t.index()] += 1;
        }
    }
    let barrels: Vec<_> = graph
        .iter()
        .filter(|(_, m)| !m.in_node_modules())
        .filter_map(|(id, m)| reexported_modules(m).map(|n| (id, n)))
        .collect();
    let ids: Vec<_> = barrels.iter().map(|(id, _)| *id).collect();
    let counts = graph.reachable_counts(&ids);
    let findings: Vec<Finding> = barrels
        .iter()
        .zip(counts)
        .map(|(&(id, reexports), count)| {
            let barrel = Barrel::new(reexports, importers[id.index()]);
            Finding::new(graph.module(id).path.clone(), FindingKind::Barrel(barrel), count)
        })
        .collect();
    info!("{BARRELS_RULE} check complete. Found {} barrel files", findings.len());
    analysis.finish(policy, findings)
}

/// How many modules `module` re-exports from, when it is a barrel: it
/// forwards at least two modules (`export * from`, `export { a } from`, or
/// an imported binding exported again), and has fewer exports of its own
/// than forwarding exports.
fn reexported_modules(module: &Module) -> Option<usize> {
    let mut from: HashSet<&str> = module.uses.reexport_all.iter().map(String::as_str).collect();
    let mut forwarding = module.uses.reexport_all.len();
    let mut own = 0;
    for export in module.exports.iter().filter(|e| !e.type_only) {
        match &export.forwarded {
            Some(f) => {
                from.insert(&f.from);
                forwarding += 1;
            }
            None => own += 1,
        }
    }
    (from.len() >= 2 && own < forwarding).then_some(from.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxiclean_core::CommonArgs;
    use std::{fs, path::Path};
    use tempfile::TempDir;

    fn write(root: &Path, path: &str, content: &str) {
        let p = root.join(path);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    /// (file, value, re-exported modules, importers) of each finding.
    fn run(root: &Path, threshold: usize) -> Vec<(String, usize, usize, usize)> {
        let config = format!(
            r#"{{ "entries": ["src/main.ts"], "rules": {{ "barrels": ["error", {{ "threshold": {threshold} }}] }} }}"#
        );
        write(root, ".oxicleanrc.json", &config);
        let result = run_barrels_check(&Config { common: CommonArgs::with_root(root) }).unwrap();
        result
            .findings
            .iter()
            .map(|f| {
                let FindingKind::Barrel(b) = &f.kind else { panic!("{f:?}") };
                let file = f.file.strip_prefix(&result.root).unwrap().to_string_lossy();
                (file.replace('\\', "/"), f.value, b.reexports, b.importers)
            })
            .collect()
    }

    #[test]
    fn scores_barrels_by_what_they_pull_in() {
        let t = TempDir::new().unwrap();
        let root = t.path();
        write(root, "src/main.ts", "import { a } from './ui';\nimport { x } from './utils';");
        write(
            root,
            "src/ui/index.ts",
            "export * from './a';\nexport { b } from './b';\nexport type { C } from './c';",
        );
        write(root, "src/ui/a.ts", "import './deep'; export const a = 1;");
        write(root, "src/ui/deep.ts", "export {};");
        write(root, "src/ui/b.ts", "export const b = 1;");
        write(root, "src/ui/c.ts", "export type C = 1;");
        // Mostly its own code: not a barrel.
        write(
            root,
            "src/utils.ts",
            "export { p } from './p';\nexport { q } from './q';\nexport const x = 1;\nexport const y = 2;\nexport const z = 3;",
        );
        write(root, "src/p.ts", "export const p = 1;");
        write(root, "src/q.ts", "export const q = 1;");

        // index, a, deep and b, from two modules: the type-only re-export
        // pulls in nothing.
        assert_eq!(run(root, 1), [("src/ui/index.ts".to_string(), 4, 2, 1)]);
        assert!(run(root, 5).is_empty());
    }
}
