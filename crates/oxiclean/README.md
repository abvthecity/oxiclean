# Oxiclean CLI

Unified command-line interface for code quality and analysis tools for JavaScript/TypeScript projects.

## Installation

```bash
# From the oxiclean monorepo
cargo install --path crates/oxiclean

# Or build and run directly
cargo run -- --help
```

## Usage

```bash
oxiclean <COMMAND> [OPTIONS]
```

## Commands

Each command runs a group of rules; `.oxicleanrc.json` turns each rule on
or off, and a rule that is off doesn't run.

| Command | Rules |
|---|---|
| `unused` | `unused-exports`, `unused-types`, `unused-enum-members`, `unused-namespace-members` |
| `dependencies` | `dependencies`, `heavy-dependencies` (`warn` by default), `catalog`, `duplicate-versions` |
| `typecheck` | `program-files`, `overlap`, `references`, `tsconfig`, `ambient-types`, `isolated-declarations`, `large-literals`, `type-bloat`, `complex-types` |
| `bloat` | `bloat`, `barrels` |
| `depth`, `cycles`, `duplicates` | the rule of the same name |
| `trace` | `hot-spots`, `duplicate-packages` (both `warn` by default); reads an existing `tsc --generateTrace` directory |
| `lint` | `lint-config` (`warn` by default) |

```bash
oxiclean unused --root ./my-project
```

**Options:**
- `--root <PATH>` - Root directory to analyze (default: git root)
- `-c, --config <FILE>` - Config file (default: `.oxicleanrc.json` in the root)
- `--format <text|json|sarif>` - Output format
- `--fix` - Apply the fixes the findings of every rule offer

Thresholds, severities, entry files and everything else are set in
`.oxicleanrc.json`; see the [main README](../../README.md#configuration).

## Adding New Rules

1. Add the rule to a library crate, with a function that runs it and
   returns a `CheckResult`; it passes its default threshold and severity
   to `policy`
2. Add its name to `Rules` in `crates/oxiclean_core/src/settings.rs`
3. Add a `(rule, style, run)` entry to the rule table of its command (such
   as `RULES` in `crates/oxiclean_typecheck/src/command.rs`);
   `oxiclean_core::run_command` skips the rule when the config turns it
   off, and records its error, without stopping the others, when it fails

A new command is a variant of the `Commands` enum in
`crates/oxiclean/src/main.rs` whose `run` returns a `CommandResult`, built
with `oxiclean_core::run_command` from a table of its rules.

## License

MIT
