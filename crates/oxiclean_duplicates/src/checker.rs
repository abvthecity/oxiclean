use std::collections::{HashMap, HashSet};

use oxiclean_core::{
    Analysis, BuildOptions, CheckResult, DuplicateExport, Export, ExportForm, ExportName, Finding,
    FindingKind, Result, RulePolicy,
};
use tracing::info;

use crate::config::{Config, DEFAULT_THRESHOLD, RULE};

/// Runs the duplicate exports check described by `cfg`.
///
/// # Errors
/// Fails if the project root can't be found, the config file is invalid, or
/// no entry files are found.
pub fn run_duplicates_check(cfg: &Config) -> Result<CheckResult> {
    info!("Starting duplicate exports check");
    // Declaration files and the files reached only through type imports
    // export too.
    let analysis = cfg
        .common
        .analyze_with(BuildOptions::default().resolve_passive(true).read_exports(true))?;
    let policy = analysis.policy(RULE, DEFAULT_THRESHOLD)?;
    Ok(check(&analysis, &policy))
}

/// Reports each module that exports the same thing under more than one
/// name, one finding per original export. See [`duplicates`] for what counts.
/// Files inside `node_modules` are left out.
#[must_use]
pub fn check(analysis: &Analysis, policy: &RulePolicy) -> CheckResult {
    if policy.min_threshold().is_none() {
        info!("{} is disabled", RULE);
        return analysis.finish(policy, Vec::new());
    }
    let graph = &analysis.graph;
    let findings: Vec<Finding> = graph
        .modules()
        .flat_map(|module| {
            // An import is from the project when it resolved to a file
            // outside node_modules.
            let in_project = |request: &str| {
                module
                    .edges
                    .iter()
                    .chain(&module.passive_edges)
                    .any(|e| e.request == request && !graph.module(e.target).in_node_modules())
            };
            let groups = if module.in_node_modules() {
                Vec::new()
            } else {
                duplicates(&module.exports, in_project)
            };
            groups.into_iter().map(|group| {
                let names: Vec<ExportName> =
                    group.iter().map(|e| ExportName::new(e.name.clone(), e.line)).collect();
                let n = names.len();
                Finding::new(
                    module.path.clone(),
                    FindingKind::DuplicateExport(DuplicateExport::new(names)),
                    n,
                )
            })
        })
        .collect();

    info!("Duplicate exports check complete. Found {} duplicates", findings.len());
    analysis.finish(policy, findings)
}

/// Groups the exports that repeat another export of the same module, as
/// knip's `duplicates` does:
///
/// - `export const b = a`, where `a` is exported and its export is neither a
///   re-export with `from` nor an import from the project passed on
///   ([`Export::forwarded`] from a module for which `in_project` is true);
/// - `export default a`, where `a` is exported in any way except
///   `export { a }`.
///
/// Names whose comment has an `@alias` tag don't count. Each group lists the
/// original export first, then the names that repeat it, in source order.
/// Unlike knip, the original may be exported after the name that repeats it.
#[must_use]
pub fn duplicates(exports: &[Export], in_project: impl Fn(&str) -> bool) -> Vec<Vec<&Export>> {
    // The first export of each name, and of each name that may be the
    // original of `export const b = a`.
    let mut first: HashMap<&str, &Export> = HashMap::new();
    let mut first_local: HashMap<&str, &Export> = HashMap::new();
    let mut in_specifier: HashSet<&str> = HashSet::new();
    for e in exports {
        first.entry(&e.name).or_insert(e);
        let forwarded_in_project = e.forwarded.as_ref().is_some_and(|f| in_project(&f.from));
        if e.form != ExportForm::ReExport && !forwarded_in_project {
            first_local.entry(&e.name).or_insert(e);
        }
        if e.form == ExportForm::Specifier {
            in_specifier.insert(&e.name);
        }
    }

    let mut groups: Vec<Vec<&Export>> = Vec::new();
    let mut group_of: HashMap<&str, usize> = HashMap::new();
    for e in exports {
        let Some(target) = e.alias_of.as_deref() else { continue };
        if e.has_tag("alias") {
            continue;
        }
        let original = match e.form {
            ExportForm::Default if !in_specifier.contains(target) => first.get(target),
            ExportForm::Declaration => first_local.get(target),
            _ => None,
        };
        let Some(&original) = original else { continue };
        match group_of.get(original.name.as_str()) {
            Some(&g) => groups[g].push(e),
            None => {
                group_of.insert(&original.name, groups.len());
                groups.push(vec![original, e]);
            }
        }
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxiclean_core::parse_imports;

    /// The groups `duplicates` finds in `src`, as lists of names.
    fn groups(src: &str) -> Vec<Vec<String>> {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("m.ts");
        std::fs::write(&file, src).unwrap();
        let exports = parse_imports(&file).unwrap().exports;
        // Relative imports are from the project; packages aren't.
        duplicates(&exports, |m| m.starts_with('.'))
            .into_iter()
            .map(|g| g.into_iter().map(|e| e.name.clone()).collect())
            .collect()
    }

    /// Each case was checked against knip 6.41.
    #[test]
    fn matches_knip() {
        let cases: &[(&str, &[&[&str]])] = &[
            ("export const a = 1;\nexport const b = a;", &[&["a", "b"]]),
            ("export let a = 1;\nexport default a;", &[&["a", "default"]]),
            ("export const a = 1;\nexport const b = a, c = a;", &[&["a", "b", "c"]]),
            // An import passed on is a re-export, so nothing repeats it...
            ("import { x } from './d';\nexport { x };\nexport const b = x;", &[]),
            ("import { x } from './d';\nexport const a = x;\nexport const b = a;", &[]),
            ("import * as ns from './d';\nexport { ns };\nexport const ns2 = ns;", &[]),
            // ...from the project. An import from a package is an export of
            // this module.
            ("import { x } from 'pkg';\nexport { x };\nexport const b = x;", &[&["x", "b"]]),
            // ...except for a default export, which knip checks against any
            // export of that name.
            (
                "import { x } from './d';\nexport { x } from './d';\nexport default x;",
                &[&["x", "default"]],
            ),
            ("function h() {}\nexport { h };\nexport default h;", &[]),
            ("export const a = 1;\nexport { a as b };", &[]),
            ("export const a = 1;\n/* @alias */\nexport default a;", &[]),
            ("export const a = 1; /** @alias */\nexport const b = a;", &[]),
            ("export const a = 1;\n/** @alias */\n// note\nexport const b = a;", &[]),
            (
                "export const a = 1;\n/** @alias */\n/** Docs. */\nexport const b = a;",
                &[&["a", "b"]],
            ),
            (
                "export function f(): void;\nexport function f(x?: number) {}\nexport const g = f;",
                &[&["f", "g"]],
            ),
            ("const a = 1;\nexport = a;", &[]),
            ("export const a = 1;\nexport const b = (a);", &[]),
        ];
        for (src, want) in cases {
            let want: Vec<Vec<String>> =
                want.iter().map(|g| g.iter().map(|n| (*n).to_string()).collect()).collect();
            assert_eq!(groups(src), want, "{src}");
        }
    }

    #[test]
    fn originals_may_come_later() {
        assert_eq!(groups("export const b = a;\nexport const a = 1;"), [["a", "b"]]);
    }
}
