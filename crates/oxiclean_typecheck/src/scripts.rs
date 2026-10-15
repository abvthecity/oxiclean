//! Which tsconfigs package.json scripts run `tsc` on.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use oxiclean_core::Project;
use serde_json::Value;

/// How a script runs `tsc` on a config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum TscRun {
    /// `tsc --noEmit`: a check of one program, from nothing unless the
    /// config keeps a `.tsbuildinfo`.
    Check,
    /// `tsc` or `tsc -p <config>`: one program, checked from nothing
    /// unless the config keeps a `.tsbuildinfo`, and emitted unless it
    /// sets `noEmit`.
    Program,
    /// `tsc -b`, or `tsc --incremental`: runs that reuse earlier work
    /// whatever the config says.
    Build,
}

/// Compilers that read a tsconfig the way `tsc` does.
const COMPILERS: &[&str] = &["tsc", "tsgo", "vue-tsc"];

/// Words that run the command after them: package runners, and
/// `cross-env` before its variables.
const RUNNERS: &[&str] = &["npx", "bunx", "pnpx", "pnpm", "yarn", "bun", "exec", "x", "cross-env"];

/// Flags after which `tsc` doesn't run a routine check of a config: it
/// prints, creates one, watches (keeping its state in memory), or is
/// being debugged.
const NOT_A_CHECK: &[&str] = &[
    "--generateTrace",
    "--extendedDiagnostics",
    "--diagnostics",
    "--explainFiles",
    "--traceResolution",
    "-v",
    "--version",
    "-h",
    "--help",
    "--all",
    "--init",
    "--showConfig",
    "--listFilesOnly",
    "-w",
    "--watch",
];

/// The configs package.json scripts of the root and every workspace
/// package run `tsc` (or `tsgo` or `vue-tsc`) on, and how. A config run
/// more than one way counts as the last of [`TscRun`]'s variants among
/// them: built over emitted over checked.
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
                *entry = (*entry).max(run);
            }
        }
    }
    out
}

/// The commands in `script`: split at `&&`, `||`, `;`, `|` and `&`, with
/// each quoted string (as `concurrently "tsc -b" "tsc -p a.json"` passes
/// commands) a command of its own.
fn commands(script: &str) -> Vec<Vec<String>> {
    let mut out = vec![Vec::new()];
    let mut word = String::new();
    let mut chars = script.chars().peekable();
    let flush = |word: &mut String, cmd: &mut Vec<String>| {
        if !word.is_empty() {
            cmd.push(std::mem::take(word));
        }
    };
    while let Some(c) = chars.next() {
        match c {
            '"' | '\'' => {
                let quoted: String = chars.by_ref().take_while(|q| *q != c).collect();
                let inner = commands(&quoted);
                // A quoted string that runs commands is a command of its
                // own; any other, such as a path with a space, is a word.
                let runs_one = inner.first().and_then(|c| {
                    c.iter().find(|w| !w.contains('=') && !RUNNERS.contains(&w.as_str()))
                });
                if inner.len() > 1 || runs_one.is_some_and(|w| is_tsc(w) || w == "cd") {
                    if let Some(cmd) = out.last_mut() {
                        flush(&mut word, cmd);
                    }
                    out.extend(inner);
                    out.push(Vec::new());
                } else {
                    word.push_str(&quoted);
                }
            }
            '&' | '|' | ';' => {
                if let Some(cmd) = out.last_mut() {
                    flush(&mut word, cmd);
                }
                while chars.peek().is_some_and(|n| matches!(n, '&' | '|')) {
                    chars.next();
                }
                out.push(Vec::new());
            }
            c if c.is_whitespace() => {
                if let Some(cmd) = out.last_mut() {
                    flush(&mut word, cmd);
                }
            }
            c => word.push(c),
        }
    }
    if let Some(cmd) = out.last_mut() {
        flush(&mut word, cmd);
    }
    out.retain(|c| !c.is_empty());
    out
}

/// The configs one script runs `tsc` on, from `dir`, following `cd`.
fn runs_in(script: &str, dir: &Path) -> Vec<(PathBuf, TscRun)> {
    let mut out = Vec::new();
    let mut cwd = dir.to_path_buf();
    for words in commands(script) {
        // The command word: past variable assignments and package runners.
        let Some(at) =
            words.iter().position(|w| !w.contains('=') && !RUNNERS.contains(&w.as_str()))
        else {
            continue;
        };
        let (command, args) = (&words[at], &words[at + 1..]);
        if command == "cd" {
            if let Some(to) = args.first() {
                cwd = path_clean::clean(cwd.join(to));
            }
            continue;
        }
        if !is_tsc(command) || args.iter().any(|a| NOT_A_CHECK.contains(&a.as_str())) {
            continue;
        }
        let build = args.iter().any(|a| a == "-b" || a == "--build");
        let incremental = args.iter().any(|a| a == "-i" || a == "--incremental");
        let mut configs = Vec::new();
        let mut files = false;
        let mut i = 0;
        while i < args.len() {
            let a = args[i].as_str();
            if a == "-p" || a == "--project" {
                configs.extend(args.get(i + 1));
                i += 2;
                continue;
            }
            if build && !a.starts_with('-') {
                configs.push(&args[i]);
            } else if is_source_file(a) {
                files = true;
            }
            i += 1;
        }
        // Files on the command line make `tsc` ignore tsconfig files.
        if files && configs.is_empty() {
            continue;
        }
        let run = if build || incremental {
            TscRun::Build
        } else if args.iter().any(|a| a == "--noEmit") {
            TscRun::Check
        } else {
            TscRun::Program
        };
        let default = String::from("tsconfig.json");
        if configs.is_empty() {
            configs.push(&default);
        }
        out.extend(configs.into_iter().map(|c| {
            let p = path_clean::clean(cwd.join(c));
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

/// Whether `word` runs a compiler that reads tsconfig files.
fn is_tsc(word: &str) -> bool {
    let name = word.rsplit_once('/').map_or(word, |(_, n)| n);
    COMPILERS.contains(&name)
}

/// Whether `word` names a TypeScript or JavaScript file.
fn is_source_file(word: &str) -> bool {
    [".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs"].iter().any(|e| word.ends_with(e))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runs(script: &str) -> Vec<(String, TscRun)> {
        runs_in(script, Path::new("/p"))
            .into_iter()
            .map(|(p, r)| (p.to_string_lossy().into_owned(), r))
            .collect()
    }

    fn program(p: &str) -> (String, TscRun) {
        (p.to_string(), TscRun::Program)
    }

    fn build(p: &str) -> (String, TscRun) {
        (p.to_string(), TscRun::Build)
    }

    #[test]
    fn reads_tsc_commands() {
        assert_eq!(runs("tsc --noEmit"), [(String::from("/p/tsconfig.json"), TscRun::Check)]);
        assert_eq!(
            runs("rimraf dist && tsc -p tsconfig.build.json"),
            [program("/p/tsconfig.build.json")]
        );
        assert_eq!(
            runs("tsc -b packages/a packages/b/tsconfig.lib.json --verbose"),
            [build("/p/packages/a/tsconfig.json"), build("/p/packages/b/tsconfig.lib.json")]
        );
        assert_eq!(
            runs("node_modules/.bin/tsgo --project ./src"),
            [program("/p/src/tsconfig.json")]
        );
        assert_eq!(runs("cross-env NODE_ENV=x pnpm exec vue-tsc"), [program("/p/tsconfig.json")]);
        assert_eq!(runs("cd packages/a && tsc"), [program("/p/packages/a/tsconfig.json")]);
        assert_eq!(
            runs(r#"concurrently "tsc -b" "tsc -p a.json""#),
            [build("/p/tsconfig.json"), program("/p/a.json")]
        );
        assert_eq!(runs("tsc --incremental -p x"), [build("/p/x/tsconfig.json")]);
        assert_eq!(runs(r#"tsc -p "my app/tsconfig.json""#), [program("/p/my app/tsconfig.json")]);
    }

    #[test]
    fn skips_what_checks_no_config() {
        for script in [
            "eslint . && vitest",
            "echo building with tsc -p x",
            "tsc -p x --version",
            "tsc --init",
            "tsc -p x --showConfig",
            "tsc -w",
            "tsc -p x --generateTrace trace",
            "tsc src/index.ts --outDir dist",
        ] {
            assert!(runs(script).is_empty(), "{script}");
        }
    }
}
