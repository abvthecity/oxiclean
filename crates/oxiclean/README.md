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
| `typecheck` | `program-files`, `overlap`, `references`, `tsconfig`, `ambient-types`, `isolated-declarations`, `large-literals`, `type-bloat` |
| `bloat` | `bloat`, `barrels` |
| `depth`, `cycles`, `duplicates` | the rule of the same name |

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
   returns a `CheckResult`
2. Add its name to `Rules` in `crates/oxiclean_core/src/settings.rs`, and
   its default severity to `default_severity` if it isn't `error`
3. Run it from the command function of its crate (such as
   `run_typecheck_command`), which skips it when the config turns it off

A new command is a variant of the `Commands` enum in
`crates/oxiclean/src/main.rs` whose `run` returns a `CommandResult`.

## License

MIT
