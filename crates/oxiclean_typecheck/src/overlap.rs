//! `overlap`: files that more than one tsconfig program type-checks.
//!
//! Each program parses, binds and checks its files on its own, so a file
//! in two programs is checked twice: by `tsc -b`, in the editor, and by
//! typed linting. Typical causes are a root tsconfig whose `include`
//! reaches into packages that have their own, and a catch-all
//! `tsconfig.eslint.json`.
//!
//! Configs where one extends the other are variants of one project (a
//! `tsconfig.build.json` that leaves out tests, say), and configs in the
//! same directory are usually split on purpose (app and node code in a
//! Vite template), so neither pair is reported.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use oxiclean_core::{CheckResult, Finding, FindingKind, Issue, Related, Result};
use tracing::info;

use crate::{
    config::{Config, DEFAULT_THRESHOLD, OVERLAP_RULE},
    tsconfig::{TsConfigFile, TsConfigs, relative},
};

/// How many shared files a finding lists.
const SAMPLES: usize = 3;

/// Runs the `overlap` check with `cfg`.
///
/// # Errors
/// Fails if the root can't be found or read, or the config file is invalid.
pub fn run_overlap_check(cfg: &Config) -> Result<CheckResult> {
    info!("Starting {OVERLAP_RULE} check");
    let workspace = cfg.common.workspace()?;
    let policy = workspace.policy(OVERLAP_RULE, DEFAULT_THRESHOLD)?;
    let configs = TsConfigs::load(&workspace.project);
    let findings = check(&configs);
    info!("{OVERLAP_RULE} check complete. Found {} findings", findings.len());
    Ok(workspace.finish(&policy, findings, configs.programs().count()))
}

/// One finding per pair of programs that share files.
#[must_use]
pub fn check(configs: &TsConfigs) -> Vec<Finding> {
    let programs: Vec<&TsConfigFile> = configs.programs().collect();
    let mut owners: BTreeMap<&Path, Vec<usize>> = BTreeMap::new();
    for (i, p) in programs.iter().enumerate() {
        for f in &p.root_files {
            owners.entry(f.as_path()).or_default().push(i);
        }
    }
    // Shared files by the program the finding goes on and the directory
    // of the other: variants of one project in one directory (a
    // `tsconfig.json` and its `tsconfig.build.json`) make one finding.
    let mut shared: BTreeMap<(usize, &Path), Shared<'_>> = BTreeMap::new();
    for (file, list) in &owners {
        for (n, &a) in list.iter().enumerate() {
            for &b in &list[n + 1..] {
                if related(programs[a], programs[b]) {
                    continue;
                }
                let (reporter, other) = reporter(a, b, &programs);
                let entry = shared.entry((reporter, programs[other].dir.as_path())).or_default();
                entry.others.insert(other);
                entry.files.insert(file);
            }
        }
    }
    shared
        .into_iter()
        .map(|((reporter, _), Shared { others, files })| {
            let others: Vec<&TsConfigFile> = others.into_iter().map(|o| programs[o]).collect();
            let files: Vec<&Path> = files.into_iter().collect();
            finding(programs[reporter], &others, &files)
        })
        .collect()
}

/// The programs a program shares files with, and the files.
#[derive(Debug, Default)]
struct Shared<'a> {
    others: BTreeSet<usize>,
    files: BTreeSet<&'a Path>,
}

/// Whether `a` and `b` are variants of one project: one extends the other,
/// or they are in the same directory.
fn related(a: &TsConfigFile, b: &TsConfigFile) -> bool {
    a.dir == b.dir || a.ancestors.contains(&b.path) || b.ancestors.contains(&a.path)
}

/// Which of two overlapping programs the finding goes on, then the other:
/// the one that reaches into the other's directory, else the later path.
fn reporter(a: usize, b: usize, programs: &[&TsConfigFile]) -> (usize, usize) {
    let (pa, pb) = (programs[a], programs[b]);
    if pb.dir.starts_with(&pa.dir) {
        (a, b)
    } else if pa.dir.starts_with(&pb.dir) || pa.path > pb.path {
        (b, a)
    } else {
        (a, b)
    }
}

fn finding(reporter: &TsConfigFile, others: &[&TsConfigFile], files: &[&Path]) -> Finding {
    let names: Vec<String> = others.iter().map(|o| relative(&reporter.dir, &o.path)).collect();
    let message = format!(
        "type-checks files that {} also type-check{}, so they are checked more than once",
        names.join(" and "),
        if names.len() == 1 { "s" } else { "" }
    );
    let mut related: Vec<Related> = others
        .iter()
        .map(|o| Related::new(o.path.clone(), include_line(o), "also includes them"))
        .collect();
    related.extend(
        files.iter().take(SAMPLES).map(|f| Related::new(PathBuf::from(f), 0, "checked by each")),
    );
    let line = include_line(reporter);
    let key = relative(&reporter.dir, &others[0].dir);
    let issue = Issue::new("shared-files", key, message, line).with_related(related);
    Finding::new(reporter.path.clone(), FindingKind::Issue(issue), files.len())
}

fn include_line(c: &TsConfigFile) -> u32 {
    ["include", "files", "extends"].iter().map(|k| c.line_of(&[k])).find(|l| *l > 0).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxiclean_core::Project;
    use std::fs;
    use tempfile::TempDir;

    fn dirs(configs: &TsConfigs) -> BTreeSet<PathBuf> {
        configs.programs().map(|p| p.dir.clone()).collect()
    }

    fn write(root: &Path, path: &str, text: &str) {
        let p = root.join(path);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    #[test]
    fn reports_cross_directory_overlap_only() {
        let t = TempDir::new().unwrap();
        let root = t.path().canonicalize().unwrap();
        write(&root, "tsconfig.json", r#"{ "include": ["packages"] }"#);
        write(&root, "packages/a/tsconfig.json", "{}");
        write(&root, "packages/a/tsconfig.build.json", r#"{ "extends": "./tsconfig.json" }"#);
        write(&root, "packages/a/tsconfig.node.json", r#"{ "include": ["vite.config.ts"] }"#);
        write(&root, "packages/a/src/x.ts", "");
        write(&root, "packages/a/vite.config.ts", "");
        let configs = TsConfigs::load(&Project::load(&root));
        assert_eq!(dirs(&configs).len(), 2);
        let findings = check(&configs);
        let summary: Vec<(String, usize)> =
            findings.iter().map(|f| (relative(&root, &f.file), f.value)).collect();
        // The root reaches into a's files; a's three programs, in one
        // directory, make one finding, and aren't compared with each other.
        assert_eq!(summary, [("tsconfig.json".to_string(), 2)]);
    }
}
