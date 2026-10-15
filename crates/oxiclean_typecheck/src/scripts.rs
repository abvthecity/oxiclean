//! Which tsconfigs package.json scripts run `tsc` on.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use oxiclean_core::Project;
use serde_json::Value;

/// How a script runs `tsc` on a config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TscRun {
    /// `tsc` or `tsc -p <config>`: one program.
    Program,
    /// `tsc -b`: a build of the config and what it references.
    Build,
}

/// The configs package.json scripts of the root and every workspace
/// package run `tsc` (or `tsgo`, TypeScript's native compiler) on, and how.
/// A config both built and run as a program counts as built.
pub(crate) fn tsc_runs(project: &Project) -> BTreeMap<PathBuf, TscRun> {
    let mut dirs = vec![project.root.clone()];
    dirs.extend(project.workspace_packages().into_iter().map(|(_, d)| d.to_path_buf()));
    let mut out = BTreeMap::new();
    for dir in dirs {
        let Some(pkg) = project.package_json(&dir) else { continue };
        let Some(scripts) = pkg.get("scripts").and_then(Value::as_object) else { continue };
        for script in scripts.values().filter_map(Value::as_str) {
            for (config, run) in runs_in(script, &dir) {
                let entry = out.entry(config).or_insert(run);
                if run == TscRun::Build {
                    *entry = TscRun::Build;
                }
            }
        }
    }
    out
}

/// The configs one script runs `tsc` on, relative to `dir`.
fn runs_in(script: &str, dir: &Path) -> Vec<(PathBuf, TscRun)> {
    let mut out = Vec::new();
    for command in script.split(['&', ';', '|']) {
        let words: Vec<&str> =
            command.split_whitespace().map(|w| w.trim_matches(|c| c == '"' || c == '\'')).collect();
        let Some(at) = words.iter().position(|w| is_tsc(w)) else { continue };
        let args = &words[at + 1..];
        let build = args.iter().any(|a| *a == "-b" || *a == "--build");
        let mut configs = Vec::new();
        let mut i = 0;
        while i < args.len() {
            match args[i] {
                "-p" | "--project" => {
                    if let Some(p) = args.get(i + 1) {
                        configs.push(*p);
                    }
                    i += 2;
                    continue;
                }
                a if build && !a.starts_with('-') => configs.push(a),
                _ => {}
            }
            i += 1;
        }
        if configs.is_empty() {
            configs.push("tsconfig.json");
        }
        let run = if build { TscRun::Build } else { TscRun::Program };
        out.extend(configs.into_iter().map(|c| {
            let p = path_clean::clean(dir.join(c));
            let p = if p.extension().is_some_and(|e| e == "json") {
                p
            } else {
                p.join("tsconfig.json")
            };
            (p, run)
        }));
    }
    out
}

/// Whether `word` runs the TypeScript compiler.
fn is_tsc(word: &str) -> bool {
    let name = word.rsplit('/').next().unwrap_or(word);
    name == "tsc" || name == "tsgo"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_tsc_commands() {
        let dir = Path::new("/p");
        assert_eq!(
            runs_in("tsc --noEmit", dir),
            [(PathBuf::from("/p/tsconfig.json"), TscRun::Program)]
        );
        assert_eq!(
            runs_in("rimraf dist && tsc -p tsconfig.build.json", dir),
            [(PathBuf::from("/p/tsconfig.build.json"), TscRun::Program)]
        );
        assert_eq!(
            runs_in("tsc -b packages/a packages/b/tsconfig.lib.json --verbose", dir),
            [
                (PathBuf::from("/p/packages/a/tsconfig.json"), TscRun::Build),
                (PathBuf::from("/p/packages/b/tsconfig.lib.json"), TscRun::Build)
            ]
        );
        assert_eq!(
            runs_in("node_modules/.bin/tsgo --project ./src", dir),
            [(PathBuf::from("/p/src/tsconfig.json"), TscRun::Program)]
        );
        assert!(runs_in("eslint . && vitest", dir).is_empty());
    }
}
