//! Git hooks: the commands husky's hooks, `simple-git-hooks` and
//! lint-staged run, read as package.json scripts are.

use std::{fs, path::Path};

use serde_json::Value;

use super::{Plugin, config};

pub(super) struct Husky;

impl Plugin for Husky {
    fn name(&self) -> &'static str {
        "husky"
    }

    fn enablers(&self) -> &'static [&'static str] {
        &["husky", "simple-git-hooks"]
    }

    /// Each command line of the hook files in `.husky`, and each hook of
    /// package.json `simple-git-hooks`.
    fn scripts(&self, dir: &Path) -> Vec<String> {
        let mut out = Vec::new();
        if let Ok(entries) = fs::read_dir(dir.join(".husky")) {
            let mut hooks: Vec<_> =
                entries.filter_map(Result::ok).map(|e| e.path()).filter(|p| p.is_file()).collect();
            hooks.sort();
            for hook in hooks {
                let Ok(text) = fs::read_to_string(&hook) else { continue };
                out.extend(
                    text.lines()
                        .map(str::trim)
                        .filter(|l| !l.is_empty() && !l.starts_with('#'))
                        // husky 4–8's `. "$(dirname -- "$0")/_/husky.sh"`.
                        .filter(|l| !l.starts_with(". ") && !l.contains("husky.sh"))
                        .map(str::to_owned),
                );
            }
        }
        if let Some(Value::Object(hooks)) = config::package_field(dir, "simple-git-hooks") {
            out.extend(hooks.values().filter_map(Value::as_str).map(str::to_owned));
        }
        out
    }
}

pub(super) struct LintStaged;

const LINT_STAGED_FILES: &[&str] = &[
    ".lintstagedrc",
    ".lintstagedrc.json",
    ".lintstagedrc.js",
    ".lintstagedrc.cjs",
    ".lintstagedrc.mjs",
    "lint-staged.config.js",
    "lint-staged.config.cjs",
    "lint-staged.config.mjs",
    "lint-staged.config.ts",
];

impl Plugin for LintStaged {
    fn name(&self) -> &'static str {
        "lint-staged"
    }

    fn enablers(&self) -> &'static [&'static str] {
        &["lint-staged"]
    }

    /// The commands of each glob in lint-staged's config, given as a string
    /// or an array of strings.
    fn scripts(&self, dir: &Path) -> Vec<String> {
        let configs = LINT_STAGED_FILES
            .iter()
            .map(|n| dir.join(n))
            .filter(|p| p.is_file())
            .filter_map(|p| config::load(&p))
            .chain(config::package_field(dir, "lint-staged"));
        let mut out = Vec::new();
        for c in configs {
            let Value::Object(globs) = c else { continue };
            for commands in globs.values() {
                match commands {
                    Value::String(s) => out.push(s.clone()),
                    Value::Array(a) => {
                        out.extend(a.iter().filter_map(Value::as_str).map(str::to_owned));
                    }
                    _ => {}
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn hook_commands() {
        let t = TempDir::new().unwrap();
        let dir = t.path();
        fs::create_dir(dir.join(".husky")).unwrap();
        fs::write(
            dir.join(".husky/pre-commit"),
            "#!/usr/bin/env sh\n. \"$(dirname -- \"$0\")/_/husky.sh\"\n\nnpx lint-staged\n",
        )
        .unwrap();
        fs::write(dir.join(".husky/commit-msg"), "commitlint --edit $1\n").unwrap();
        fs::write(
            dir.join("package.json"),
            r#"{ "lint-staged": { "*.ts": ["eslint --fix", "prettier --write"], "*.css": "stylelint" } }"#,
        )
        .unwrap();
        assert_eq!(Husky.scripts(dir), ["commitlint --edit $1", "npx lint-staged"]);
        assert_eq!(LintStaged.scripts(dir), ["stylelint", "eslint --fix", "prettier --write"]);
    }
}
