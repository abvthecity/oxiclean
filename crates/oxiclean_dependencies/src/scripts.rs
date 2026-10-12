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
/// `VAR=value` assignments, their own flags, or a `--`.
const WRAPPERS: &[&str] = &["cross-env", "cross-env-shell", "dotenv", "env-cmd"];

/// Flags of [`WRAPPERS`] that take a value, as `env-cmd -f .env`.
const WRAPPER_VALUE_FLAGS: &[&str] =
    &["-e", "-f", "-c", "-v", "-p", "-r", "--file", "--environments", "--path", "--rc-file"];

/// Commands whose `-r` loads a module. Elsewhere, as in `cp -r`, it doesn't.
const NODE_LIKE: &[&str] = &[
    "node",
    "mocha",
    "tsx",
    "ts-node",
    "ts-node-esm",
    "nodemon",
    "ava",
    "c8",
    "nyc",
    "tap",
    "electron",
    "babel-node",
    "esno",
];

/// Commands that take other commands as quoted arguments, as
/// `concurrently "tsc -w" vite` or `sh -c 'tsc && vite'`.
const SHELLS: &[&str] = &["concurrently", "sh", "bash", "zsh", "nodemon"];

/// The binaries and packages `script` uses. Commands joined by `&&`, `||`,
/// `;`, `|` or `&` are each read, with quotes as a shell reads them;
/// `VAR=value` prefixes are skipped, and `NODE_OPTIONS` read for loaders;
/// `npx`, `pnpm exec`, `yarn`, `pnpm run` and `bun run` of a name that
/// isn't a script, wrappers such as `cross-env`, and the commands
/// `concurrently` and `sh -c` take, are looked through to the binary they
/// run. `is_script` says whether a name is one of the package's scripts.
pub(crate) fn script_refs(script: &str, is_script: &dyn Fn(&str) -> bool) -> Vec<ScriptRef> {
    let mut reader = Reader { is_script, out: Vec::new() };
    reader.script(script);
    reader.out
}

struct Reader<'a> {
    is_script: &'a dyn Fn(&str) -> bool,
    out: Vec<ScriptRef>,
}

impl Reader<'_> {
    fn script(&mut self, script: &str) {
        for command in commands(script) {
            let words: Vec<&str> = command.iter().map(String::as_str).collect();
            self.command(&words);
        }
    }

    fn command(&mut self, words: &[&str]) {
        let mut words = words;
        while let [first, rest @ ..] = words
            && let Some((_, value)) = assignment(first)
        {
            // `NODE_OPTIONS='--require ts-node/register'`.
            let value: Vec<&str> = value.split_whitespace().collect();
            self.loaders(&value, true);
            words = rest;
        }
        let [bin, rest @ ..] = words else { return };
        let bin = bin.trim_start_matches("./");
        let bin = bin.strip_prefix("node_modules/.bin/").unwrap_or(bin);
        let script_ext = bin.rsplit_once('.').is_some_and(|(_, ext)| {
            matches!(ext, "sh" | "js" | "mjs" | "cjs" | "ts" | "mts" | "cts" | "py")
        });
        if script_ext || bin.contains(['/', '\\', '$']) {
            // A file in the repository, as `./scripts/build.sh`.
            return;
        }
        self.loaders(rest, NODE_LIKE.contains(&bin));
        match bin {
            "npx" | "pnpx" | "bunx" => self.runner(rest),
            "npm" | "pnpm" | "yarn" | "bun" => self.manager(bin, rest),
            _ if WRAPPERS.contains(&bin) => {
                self.out.push(ScriptRef::Binary(bin.to_string()));
                let start = match rest.iter().position(|w| *w == "--") {
                    Some(i) => i + 1,
                    None => skip_flags(rest, WRAPPER_VALUE_FLAGS, true),
                };
                self.command(&rest[start..]);
            }
            _ if SHELLS.contains(&bin) => {
                self.out.push(ScriptRef::Binary(bin.to_string()));
                for (i, arg) in rest.iter().enumerate() {
                    let after_flag = i > 0 && matches!(rest[i - 1], "-c" | "-x" | "--exec");
                    if after_flag
                        || bin == "concurrently" && !arg.starts_with('-')
                        || arg.contains(' ')
                    {
                        self.script(arg);
                    }
                }
            }
            _ => self.out.push(ScriptRef::Binary(bin.to_string())),
        }
    }

    /// `pnpm exec tsc`, `yarn tsc`, `pnpm run tsc` where `tsc` isn't a
    /// script, but not `npm test` or `pnpm install`.
    fn manager(&mut self, bin: &str, rest: &[&str]) {
        let i = skip_flags(rest, VALUE_FLAGS, false);
        let Some(sub) = rest.get(i) else { return };
        let after = &rest[i + 1..];
        if EXEC_COMMANDS.contains(sub) {
            self.runner(after);
        } else if bin == "npm" {
        } else if matches!(*sub, "run" | "run-script") {
            let j = skip_flags(after, VALUE_FLAGS, false);
            if after.get(j).is_some_and(|name| !(self.is_script)(name)) {
                self.command(&after[j..]);
            }
        } else if !MANAGER_COMMANDS.contains(sub) && !(self.is_script)(sub) {
            self.command(&rest[i..]);
        }
    }

    /// `npx [-p pkg] bin args`: the packages `-p` names, and the binary,
    /// which may carry a version, as `cowsay@1`. Running a binary through
    /// `npx` by its package's name is the usual case, so the binary's name
    /// also counts as a package.
    fn runner(&mut self, words: &[&str]) {
        let mut i = 0;
        while let Some(w) = words.get(i) {
            if matches!(*w, "-p" | "--package") {
                if let Some(p) = words.get(i + 1) {
                    self.out.push(ScriptRef::Package(without_version(p).to_string()));
                }
                i += 2;
            } else if let Some(p) = w.strip_prefix("--package=") {
                self.out.push(ScriptRef::Package(without_version(p).to_string()));
                i += 1;
            } else if w.starts_with('-') {
                i += 1;
            } else {
                let name = without_version(w);
                self.out.push(ScriptRef::Package(name.to_string()));
                self.command(&[[name].as_slice(), &words[i + 1..]].concat());
                return;
            }
        }
    }

    /// The packages `--require`, `--import` and `--loader` load, and `-r`
    /// when `short` is set.
    fn loaders(&mut self, words: &[&str], short: bool) {
        let is_flag = |f: &str| LOADER_FLAGS.contains(&f) && (short || f != "-r");
        for (i, w) in words.iter().enumerate() {
            let value = match w.split_once('=') {
                Some((flag, value)) if is_flag(flag) => Some(value),
                _ if is_flag(w) => words.get(i + 1).copied(),
                _ => None,
            };
            if let Some(v) = value.filter(|v| is_bare(v)) {
                self.out.push(ScriptRef::Package(v.to_string()));
            }
        }
    }
}

/// The index of the first word that isn't a flag, a flag's value (for
/// `value_flags`), or, with `assignments`, a `VAR=value`.
fn skip_flags(words: &[&str], value_flags: &[&str], assignments: bool) -> usize {
    let mut i = 0;
    while let Some(w) = words.get(i) {
        if w.starts_with('-') {
            i += if value_flags.contains(w) { 2 } else { 1 };
        } else if assignments && assignment(w).is_some() {
            i += 1;
        } else {
            break;
        }
    }
    i.min(words.len())
}

/// The commands of `script`, each as its words with quotes removed, as a
/// shell splits them: at `&&`, `||`, `;`, `|`, `&`, newlines and
/// parentheses outside quotes.
fn commands(script: &str) -> Vec<Vec<String>> {
    let mut commands = Vec::new();
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut chars = script.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => word.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                in_word = true;
            }
            (None, '\\') => {
                word.extend(chars.next());
                in_word = true;
            }
            (None, '&' | '|' | ';' | '\n' | '(' | ')' | '`') => {
                if std::mem::take(&mut in_word) {
                    words.push(std::mem::take(&mut word));
                }
                if !words.is_empty() {
                    commands.push(std::mem::take(&mut words));
                }
            }
            (None, c) if c.is_whitespace() => {
                if std::mem::take(&mut in_word) {
                    words.push(std::mem::take(&mut word));
                }
            }
            (None, c) => {
                word.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        words.push(word);
    }
    if !words.is_empty() {
        commands.push(words);
    }
    commands
}

/// `pkg@1.2.3` or `@scope/pkg@latest` without the version.
fn without_version(spec: &str) -> &str {
    let from = usize::from(spec.starts_with('@'));
    match spec[from..].find('@') {
        Some(i) => &spec[..from + i],
        None => spec,
    }
}

/// `VAR=value` as the name and the value.
fn assignment(word: &str) -> Option<(&str, &str)> {
    word.split_once('=').filter(|(name, _)| {
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
        script_refs(script, &|name| name == "build")
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
        assert_eq!(refs("env-cmd -f .env next build"), ["bin env-cmd", "bin next"]);
        assert_eq!(refs("dotenv -e .env.local jest"), ["bin dotenv", "bin jest"]);
    }

    #[test]
    fn run_of_a_binary() {
        assert_eq!(refs("yarn run eslint . && bun run vitest"), ["bin eslint", "bin vitest"]);
        // `build` is a script, so `pnpm run build` runs no binary.
        assert!(refs("pnpm run build && pnpm build && bun run ./x.ts").is_empty());
    }

    #[test]
    fn quoted_commands() {
        assert_eq!(
            refs("concurrently -k \"tsc -w\" vite"),
            ["bin concurrently", "bin tsc", "bin vite"]
        );
        assert_eq!(refs("sh -c 'tsc && vite'"), ["bin sh", "bin tsc", "bin vite"]);
        assert_eq!(
            refs("nodemon --exec babel-node src/index.js"),
            ["bin nodemon", "bin babel-node"]
        );
        assert_eq!(
            refs("NODE_OPTIONS='--require ts-node/register' mocha"),
            ["pkg ts-node/register", "bin mocha"]
        );
    }

    #[test]
    fn loader_flags() {
        assert_eq!(
            refs("mocha -r ts-node/register --import=tsx 'test/**/*.ts'"),
            ["pkg ts-node/register", "pkg tsx", "bin mocha"]
        );
        assert_eq!(refs("node --require ./setup.js x.js"), ["bin node"]);
        assert_eq!(refs("cp -r src dist"), ["bin cp"]);
    }
}
