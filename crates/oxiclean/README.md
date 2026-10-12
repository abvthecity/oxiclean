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

### `bloat`

Detects excessive imports that can lead to large bundle sizes.

```bash
oxiclean bloat --root ./my-project
```

**Options:**
- `--root <PATH>` - Root directory to analyze (default: git root)
- `-c, --config <FILE>` - Config file (default: `.oxicleanrc.json` in the root)
- `--format <text|json|sarif>` - Output format

Thresholds, entry files and everything else are set in `.oxicleanrc.json`;
see the [main README](../../README.md#configuration).

See [crates/oxiclean_bloat/README.md](../oxiclean_bloat/README.md) for details.

## Adding New Tools

1. Create a library crate with a `Config` struct that derives `clap::Parser`
2. Add the crate as a dependency in `crates/oxiclean/Cargo.toml`
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

// In crates/oxiclean/src/main.rs
enum Commands {
    ImportBloat(oxiclean_bloat::Config),
    MyTool(my_tool::Config),
}
```

## License

MIT
