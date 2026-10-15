use std::collections::{HashMap, HashSet};

use oxiclean_core::{
    Analysis, CheckResult, Finding, FindingKind, ModuleId, Result, RulePolicy, Severity,
};
use tracing::info;

use crate::config::{Config, DEFAULT_THRESHOLD, RULE};

/// Runs the import bloat check described by `cfg`.
///
/// # Errors
/// Fails if the project root can't be found, an entry glob is invalid, or no
/// entry files are found.
pub fn run_bloat_check(cfg: &Config) -> Result<CheckResult> {
    info!("Starting import bloat check");
    run_on(&cfg.common.analyze()?)
}

/// Runs the import bloat check on `analysis`.
pub(crate) fn run_on(analysis: &Analysis) -> Result<CheckResult> {
    let policy = analysis.policy(RULE, DEFAULT_THRESHOLD, Severity::Error)?;
    Ok(check(analysis, &policy))
}

/// Flags each entry whose whole graph, and each direct import of an entry whose
/// own graph, reaches its file's threshold of unique modules
/// under `policy`.
#[must_use]
pub fn check(analysis: &Analysis, policy: &RulePolicy) -> CheckResult {
    let Some(threshold) = policy.min_threshold() else {
        info!("{} is disabled", RULE);
        return analysis.finish(policy, Vec::new());
    };
    let graph = &analysis.graph;

    let mut queries: Vec<ModuleId> = analysis.entries.clone();
    for &entry in &analysis.entries {
        queries.extend(graph.module(entry).edges.iter().map(|e| e.target));
    }
    queries.sort_unstable();
    queries.dedup();
    let counts: HashMap<ModuleId, usize> =
        queries.iter().copied().zip(graph.reachable_counts(&queries)).collect();

    let mut findings = Vec::new();
    for &entry in &analysis.entries {
        let module = graph.module(entry);
        let mut seen = HashSet::new();
        for edge in &module.edges {
            let count = counts[&edge.target];
            if seen.insert(edge.target) && count >= threshold {
                findings.push(Finding::new(
                    module.path.clone(),
                    FindingKind::Import {
                        request: edge.request.clone(),
                        resolved: graph.module(edge.target).path.clone(),
                        line: edge.line,
                    },
                    count,
                ));
            }
        }
        if counts[&entry] >= threshold {
            findings.push(Finding::new(
                module.path.clone(),
                FindingKind::WholeGraph,
                counts[&entry],
            ));
        }
    }

    info!("Import bloat check complete. Found {} findings", findings.len());
    analysis.finish(policy, findings)
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

    fn run_with(root: &Path, entry: &str, threshold: usize, dynamic: bool) -> CheckResult {
        let config = format!(
            r#"{{ "entries": ["{entry}"], "includeDynamic": {dynamic},
                 "rules": {{ "bloat": ["error", {{ "threshold": {threshold} }}] }} }}"#
        );
        write(root, ".oxicleanrc.json", &config);
        run_bloat_check(&Config { common: CommonArgs::with_root(root) }).unwrap()
    }

    fn run(root: &Path, entry: &str, threshold: usize) -> CheckResult {
        run_with(root, entry, threshold, false)
    }

    /// (request or "<entry>", value) pairs, in report order.
    fn summary(result: &CheckResult) -> Vec<(String, usize)> {
        result
            .findings
            .iter()
            .map(|f| match &f.kind {
                FindingKind::Import { request, .. } => (request.clone(), f.value),
                _ => ("<entry>".to_string(), f.value),
            })
            .collect()
    }

    #[test]
    fn counts_unique_modules_per_import() {
        let t = TempDir::new().unwrap();
        write(t.path(), "src/index.js", "import './a'; import './b';");
        write(t.path(), "src/a.js", "import './c';");
        write(t.path(), "src/b.js", "import './c';");
        write(t.path(), "src/c.js", "// c");

        let result = run(t.path(), "src/index.js", 1);
        assert_eq!(
            summary(&result),
            vec![("<entry>".into(), 4), ("./a".into(), 2), ("./b".into(), 2)]
        );
        assert_eq!(result.files_analyzed, 4);
    }

    #[test]
    fn cycles_count_each_module_once() {
        let t = TempDir::new().unwrap();
        write(t.path(), "src/index.js", "import './a';");
        write(t.path(), "src/a.js", "import './b';");
        write(t.path(), "src/b.js", "import './a';");

        assert_eq!(
            summary(&run(t.path(), "src/index.js", 1)),
            vec![("<entry>".into(), 3), ("./a".into(), 2)]
        );
    }

    #[test]
    fn below_threshold_reports_nothing() {
        let t = TempDir::new().unwrap();
        write(t.path(), "src/index.js", "import './a';");
        write(t.path(), "src/a.js", "");

        assert!(run(t.path(), "src/index.js", 3).findings.is_empty());
    }

    #[test]
    fn dynamic_imports_are_skipped_by_default() {
        let t = TempDir::new().unwrap();
        write(t.path(), "src/index.js", "import('./lazy');");
        write(t.path(), "src/lazy.js", "import './c';");
        write(t.path(), "src/c.js", "");

        assert_eq!(summary(&run(t.path(), "src/index.js", 1)), vec![("<entry>".into(), 1)]);

        let result = run_with(t.path(), "src/index.js", 1, true);
        assert_eq!(summary(&result), vec![("<entry>".into(), 3), ("./lazy".into(), 2)]);
    }

    #[test]
    fn non_js_imports_are_leaves() {
        let t = TempDir::new().unwrap();
        write(t.path(), "src/index.js", "import './style.css';");
        write(t.path(), "src/style.css", "body { color: red }");

        let result = run(t.path(), "src/index.js", 1);
        assert_eq!(summary(&result), vec![("<entry>".into(), 2), ("./style.css".into(), 1)]);
        assert!(result.parse_failures.is_empty());
    }
}
