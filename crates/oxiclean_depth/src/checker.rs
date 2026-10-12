use std::collections::HashSet;

use oxiclean_core::{Analysis, CheckResult, Finding, FindingKind, Result, RulePolicy};
use tracing::info;

use crate::config::{Config, DEFAULT_THRESHOLD, RULE};

/// Runs the import depth check described by `cfg`.
///
/// # Errors
/// Fails if the project root can't be found, an entry glob is invalid, or no
/// entry files are found.
pub fn run_depth_check(cfg: &Config) -> Result<CheckResult> {
    info!("Starting import depth check");
    let analysis = cfg.common.analyze()?;
    let policy = analysis.policy(RULE, DEFAULT_THRESHOLD)?;
    Ok(check(&analysis, &policy))
}

/// Flags each direct import of an entry whose depth reaches its file's
/// threshold under `policy`. The
/// depth of an import is one (the import itself) plus the longest chain below
/// the imported module.
#[must_use]
pub fn check(analysis: &Analysis, policy: &RulePolicy) -> CheckResult {
    let Some(threshold) = policy.min_threshold() else {
        info!("{} is disabled", RULE);
        return analysis.finish(policy, Vec::new());
    };
    let graph = &analysis.graph;
    let depths = graph.depths();

    let mut findings = Vec::new();
    for &entry in &analysis.entries {
        let module = graph.module(entry);
        let mut seen = HashSet::new();
        for edge in &module.edges {
            let target = graph.module(edge.target);
            let depth = 1 + depths[edge.target.index()];
            if seen.insert(edge.target) && depth >= threshold {
                findings.push(Finding::new(
                    module.path.clone(),
                    FindingKind::Import {
                        request: edge.request.clone(),
                        resolved: target.path.clone(),
                        line: edge.line,
                    },
                    depth,
                ));
            }
        }
    }

    info!("Import depth check complete. Found {} findings", findings.len());
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

    /// (request, depth) pairs for the given entry glob, in report order.
    fn depths(root: &Path, entry_glob: &str) -> Vec<(String, usize)> {
        let config = format!(
            r#"{{ "entries": ["{entry_glob}"], "rules": {{ "depth": ["error", {{ "threshold": 1 }}] }} }}"#
        );
        write(root, ".oxicleanrc.json", &config);
        run_depth_check(&Config { common: CommonArgs::with_root(root) })
            .unwrap()
            .findings
            .into_iter()
            .map(|f| match f.kind {
                FindingKind::Import { request, .. } => (request, f.value),
                _ => unreachable!("depth only reports imports"),
            })
            .collect()
    }

    #[test]
    fn depth_counts_the_longest_chain() {
        let t = TempDir::new().unwrap();
        write(t.path(), "src/file.js", "import './a'; import './b';");
        write(t.path(), "src/a.js", "// a");
        write(t.path(), "src/b.js", "import './c';");
        write(t.path(), "src/c.js", "import './d';");
        write(t.path(), "src/d.js", "// d");

        assert_eq!(depths(t.path(), "src/file.js"), vec![("./b".into(), 3), ("./a".into(), 1)]);
    }

    #[test]
    fn no_imports_no_findings() {
        let t = TempDir::new().unwrap();
        write(t.path(), "src/file.js", "export const x = 1;");
        assert!(depths(t.path(), "src/file.js").is_empty());
    }

    /// Regression for the audit's cycle bug: the same import used to report
    /// depth 2 or 5 depending on which entry reached the cycle first.
    #[test]
    fn cycles_give_the_same_depth_from_every_entry() {
        let t = TempDir::new().unwrap();
        write(t.path(), "src/e.ts", "import './a'; import './b';");
        write(t.path(), "src/e2.ts", "import './b';");
        write(t.path(), "src/a.ts", "import './b'; import './x';");
        write(t.path(), "src/b.ts", "import './a';");
        write(t.path(), "src/x.ts", "import './y';");
        write(t.path(), "src/y.ts", "import './z';");
        write(t.path(), "src/z.ts", "export const z = 1;");

        // a and b form one level; below them is x -> y -> z.
        assert_eq!(depths(t.path(), "src/e.ts"), vec![("./a".into(), 4), ("./b".into(), 4)]);
        assert_eq!(depths(t.path(), "src/e2.ts"), vec![("./b".into(), 4)]);
        for _ in 0..5 {
            assert_eq!(depths(t.path(), "src/e*.ts"), {
                let mut all = vec![("./a".into(), 4), ("./b".into(), 4), ("./b".into(), 4)];
                all.sort_by(|x: &(String, usize), y| x.0.cmp(&y.0));
                all
            });
        }
    }

    /// Regression for the audit's stack overflow on a 20,000-module chain.
    #[test]
    fn deep_chain_does_not_overflow() {
        let t = TempDir::new().unwrap();
        let n = 20_000;
        for i in 0..n {
            let body = if i + 1 < n { format!("import './m{}';", i + 1) } else { String::new() };
            write(t.path(), &format!("src/m{i}.ts"), &body);
        }
        assert_eq!(depths(t.path(), "src/m0.ts"), vec![("./m1".into(), n - 1)]);
    }
}
