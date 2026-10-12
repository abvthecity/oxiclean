//! What package.json scripts run: the binaries they call, and the packages
//! they load with flags such as `node -r ts-node/register`.

/// One thing a script uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ScriptRef {
    /// A binary the script runs, as `tsc` in `tsc -b`.
    Binary(String),
    /// A package the script names, as `ts-node` in `mocha -r ts-node/register`.
    Package(String),
}

/// Flags whose value is a module to load.
const LOADER_FLAGS: &[&str] = &["-r", "--require", "--import", "--loader", "--experimental-loader"];

/// `npm`, `pnpm`, `yarn` and `bun` subcommands that run a binary: `pnpm
/// exec tsc`.
const EXEC_COMMANDS: &[&str] = &["exec", "dlx", "x"];

/// Package manager subcommands that don't run a binary of a dependency.
/// Anything else after `pnpm`, `yarn` or `bun`, as `yarn tsc`, may.
const MANAGER_COMMANDS: &[&str] = &[
    "add",
    "audit",
    "build",
    "ci",
    "config",
    "create",
    "dedupe",
    "i",
    "info",
    "init",
    "install",
    "link",
    "list",
    "ls",
    "outdated",
    "pack",
    "prune",
    "publish",
    "rebuild",
    "remove",
    "rm",
    "run",
    "run-script",
    "start",
    "test",
    "uninstall",
    "unlink",
    "up",
    "update",
    "upgrade",
    "version",
    "why",
    "workspace",
    "workspaces",
];

/// Package manager flags that take a value, as `pnpm --filter app exec`.
const VALUE_FLAGS: &[&str] = &["--filter", "-F", "--dir", "-C", "--cwd", "--prefix"];

/// Commands that run the rest of the line as another command, after any
/// `VAR=value` assignments or a `--`.
const WRAPPERS: &[&str] = &["cross-env", "cross-env-shell", "dotenv", "env-cmd"];

/// The binaries and packages `script` uses. Commands joined by `&&`, `||`,
/// `;`, `|` or `&` are each read; `VAR=value` prefixes are skipped; `npx`,
/// `pnpm exec`, `yarn` and the like, and wrappers such as `cross-env`,
/// are looked through to the binary they run.
pub(crate) fn script_refs(script: &str) -> Vec<ScriptRef> {
    let mut out = Vec::new();
    for command in script.split(['&', '|', ';', '\n', '(', ')']) {
        let words: Vec<&str> = command
            .split_whitespace()
            .map(|w| w.trim_matches(|c| c == '"' || c == '\''))
            .filter(|w| !w.is_empty())
            .collect();
        command_refs(&words, &mut out);
    }
    out
}

fn command_refs(words: &[&str], out: &mut Vec<ScriptRef>) {
    let mut words = words;
    while let [first, rest @ ..] = words
        && is_assignment(first)
    {
        words = rest;
    }
    let [bin, rest @ ..] = words else { return };
    loader_refs(rest, out);
    let bin = bin.trim_start_matches("./");
    let bin = bin.strip_prefix("node_modules/.bin/").unwrap_or(bin);
    let script_ext = bin.rsplit_once('.').is_some_and(|(_, ext)| {
        matches!(ext, "sh" | "js" | "mjs" | "cjs" | "ts" | "mts" | "cts" | "py")
    });
    if script_ext || bin.contains(['/', '\\', '$']) {
        // A file in the repository, as `./scripts/build.sh`.
        return;
    }
    match bin {
        "npx" | "pnpx" | "bunx" => runner_refs(rest, out),
        "npm" | "pnpm" | "yarn" | "bun" => {
            let mut i = 0;
            while let Some(w) = rest.get(i).filter(|w| w.starts_with('-')) {
                i += if VALUE_FLAGS.contains(w) { 2 } else { 1 };
            }
            let Some(sub) = rest.get(i) else { return };
            let after: Vec<&str> = rest[i + 1..].to_vec();
            if EXEC_COMMANDS.contains(sub) {
                runner_refs(&after, out);
            } else if bin != "npm" && !MANAGER_COMMANDS.contains(sub) {
                let mut words = vec![*sub];
                words.extend(after);
                command_refs(&words, out);
            }
        }
        _ if WRAPPERS.contains(&bin) => {
            out.push(ScriptRef::Binary(bin.to_string()));
            match rest.iter().position(|w| *w == "--") {
                Some(i) => command_refs(&rest[i + 1..], out),
                None if bin.starts_with("cross-env") => command_refs(rest, out),
                None => {}
            }
        }
        _ => out.push(ScriptRef::Binary(bin.to_string())),
    }
}

/// `npx [-p pkg] bin args`: the packages `-p` names, and the binary, which
/// may carry a version, as `cowsay@1`. Running a binary through `npx` by
/// its package's name is the usual case, so the binary's name also counts
/// as a package.
fn runner_refs(words: &[&str], out: &mut Vec<ScriptRef>) {
    let mut i = 0;
    while let Some(w) = words.get(i) {
        if matches!(*w, "-p" | "--package") {
            if let Some(p) = words.get(i + 1) {
                out.push(ScriptRef::Package(without_version(p).to_string()));
            }
            i += 2;
        } else if let Some(p) = w.strip_prefix("--package=") {
            out.push(ScriptRef::Package(without_version(p).to_string()));
            i += 1;
        } else if w.starts_with('-') {
            i += 1;
        } else {
            let name = without_version(w);
            out.push(ScriptRef::Package(name.to_string()));
            command_refs(&[[name].as_slice(), &words[i + 1..]].concat(), out);
            return;
        }
    }
}

/// The packages `-r`, `--require`, `--import` and `--loader` load.
fn loader_refs(words: &[&str], out: &mut Vec<ScriptRef>) {
    for (i, w) in words.iter().enumerate() {
        let value = match w.split_once('=') {
            Some((flag, value)) if LOADER_FLAGS.contains(&flag) => Some(value),
            _ if LOADER_FLAGS.contains(w) => words.get(i + 1).copied(),
            _ => None,
        };
        if let Some(v) = value.filter(|v| is_bare(v)) {
            out.push(ScriptRef::Package(v.to_string()));
        }
    }
}

/// `pkg@1.2.3` or `@scope/pkg@latest` without the version.
fn without_version(spec: &str) -> &str {
    let from = usize::from(spec.starts_with('@'));
    match spec[from..].find('@') {
        Some(i) => &spec[..from + i],
        None => spec,
    }
}

fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// A package name rather than a path, URL or `node:` module.
pub(crate) fn is_bare(request: &str) -> bool {
    !request.is_empty()
        && !request.starts_with(['.', '/', '#', '\\', '$'])
        && !request.contains(':')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refs(script: &str) -> Vec<String> {
        script_refs(script)
            .into_iter()
            .map(|r| match r {
                ScriptRef::Binary(b) => format!("bin {b}"),
                ScriptRef::Package(p) => format!("pkg {p}"),
            })
            .collect()
    }

    #[test]
    fn commands_and_env_prefixes() {
        assert_eq!(
            refs("NODE_ENV=production tsc -b && vite build || eslint . ; prettier -c . | tee x"),
            ["bin tsc", "bin vite", "bin eslint", "bin prettier", "bin tee"]
        );
        assert_eq!(refs("node ./scripts/build.js & ./run.sh"), ["bin node"]);
        assert_eq!(refs("node_modules/.bin/jest"), ["bin jest"]);
    }

    #[test]
    fn package_runners() {
        assert_eq!(refs("npx -y cowsay@1 hi"), ["pkg cowsay", "bin cowsay"]);
        assert_eq!(refs("npx -p @scope/cli@2 scli"), ["pkg @scope/cli", "pkg scli", "bin scli"]);
        assert_eq!(refs("pnpm exec tsc"), ["pkg tsc", "bin tsc"]);
        assert_eq!(refs("pnpm --filter app exec vitest"), ["pkg vitest", "bin vitest"]);
        assert_eq!(refs("yarn tsc --noEmit"), ["bin tsc"]);
        assert!(refs("pnpm run build && npm test && yarn install && npm i").is_empty());
        assert!(refs("npm run-script lint").is_empty());
    }

    #[test]
    fn wrappers() {
        assert_eq!(refs("cross-env NODE_ENV=test jest"), ["bin cross-env", "bin jest"]);
        assert_eq!(refs("dotenv -e .env -- next dev"), ["bin dotenv", "bin next"]);
        assert_eq!(refs("env-cmd -f .env"), ["bin env-cmd"]);
    }

    #[test]
    fn loader_flags() {
        assert_eq!(
            refs("mocha -r ts-node/register --import=tsx 'test/**/*.ts'"),
            ["pkg ts-node/register", "pkg tsx", "bin mocha"]
        );
        assert_eq!(refs("node --require ./setup.js x.js"), ["bin node"]);
    }
}
