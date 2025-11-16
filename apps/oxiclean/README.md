# Oxiclean CLI

Unified command-line interface for code quality and analysis tools for JavaScript/TypeScript projects.

## Installation

```bash
# From the oxiclean monorepo
just install

# Or build and run directly
just run-oxiclean --help
```

## Usage

```bash
oxiclean <COMMAND> [OPTIONS]
```

## Commands

### `import-bloat`

Detects excessive imports that can lead to large bundle sizes.

```bash
# Analyze a project
oxiclean import-bloat --root ./my-project --threshold 100

# Filter entry files
oxiclean import-bloat --entry-glob "apps/*/src/**/*.tsx"
```

**Options:**
- `--root <PATH>` - Root directory to analyze (default: git root)
- `--threshold <N>` - Max reachable modules before warning (default: 200)
- `--entry-glob <PATTERN>` - Glob pattern to filter entry files

See [crates/oxiclean_import_bloat/README.md](../../crates/oxiclean_import_bloat/README.md) for details.

## Adding New Tools

1. Create a library crate with a `Config` struct that derives `clap::Parser`
2. Add the crate as a dependency in `apps/oxiclean/Cargo.toml`
3. Add a new variant to the `Commands` enum
4. Handle the command in the match statement

**Example:**

```rust
// In crates/my_tool/src/lib.rs
#[derive(Debug, Clone, Parser)]
pub struct Config {
    #[arg(long)]
    pub input: String,
}

pub fn run(cfg: Config) -> anyhow::Result<()> {
    // Tool logic
    Ok(())
}

// In apps/oxiclean/src/main.rs
enum Commands {
    ImportBloat(oxiclean_import_bloat::Config),
    MyTool(my_tool::Config),
}
```

## License

MIT
