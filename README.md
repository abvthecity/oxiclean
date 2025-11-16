# Oxiclean 🧼

A Rust monorepo for code quality and analysis tools.

## Quick Start

```bash
# Install
cargo install --path apps/oxiclean

# Run import bloat checker
oxiclean import-bloat --root ./my-project --threshold 100

# Get help
oxiclean --help
oxiclean import-bloat --help
```

## Development

```bash
# Build
just build

# Run tools (from monorepo)
just run-oxiclean import-bloat --root ./project
just run-import-bloat --root ./project  # Shortcut

# Quality checks
just test      # Run tests
just lint      # Run clippy
just fmt       # Format code
just ci        # Run all checks

# Show all commands
just
```

## Project Structure

```
oxiclean/
├── apps/
│   └── oxiclean/              # Main CLI application
├── crates/
│   └── oxiclean_import_bloat/ # Import bloat detection library
├── Cargo.toml                 # Workspace configuration
└── justfile                   # Build commands
```

## Tools

### Import Bloat Checker

Detects excessive imports in JavaScript/TypeScript projects that can lead to large bundle sizes.

**Usage:**
```bash
oxiclean import-bloat --root ./my-project --threshold 50
oxiclean import-bloat --entry-glob "apps/*/src" --threshold 100
```

**See:** [crates/oxiclean_import_bloat/README.md](crates/oxiclean_import_bloat/README.md)

## Debugging & Logging

Oxiclean uses the `log` crate with `env_logger` for debug output. Enable logging via the `RUST_LOG` environment variable:

```bash
# Show all debug logs
RUST_LOG=debug oxiclean import-bloat

# Show only info level
RUST_LOG=info oxiclean import-bloat

# Show trace level (very verbose)
RUST_LOG=trace oxiclean import-bloat

# Filter by module
RUST_LOG=oxiclean_import_bloat::resolver=trace oxiclean import-bloat
```

**See:** [LOGGING.md](LOGGING.md) for detailed logging documentation.

## License

MIT
