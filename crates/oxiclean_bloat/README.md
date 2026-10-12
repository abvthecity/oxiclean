# Import Bloat Checker

Detects excessive imports in JavaScript/TypeScript projects that can lead to large bundle sizes.

## Installation

```bash
# Install oxiclean CLI
cargo install --path crates/oxiclean

# Or run from monorepo
cargo run -- bloat --help
```

## Usage

```bash
# Analyze current directory (defaults to git root)
oxiclean bloat

# Analyze another project
oxiclean bloat --root ./my-project
```

### Options

- `--root <PATH>` - Root directory to analyze (default: git root)
- `-c, --config <FILE>` - Config file (default: `.oxicleanrc.json` in the root)
- `--format <text|json|sarif>` - Output format

The threshold (default: 300 modules), entry files and ignore patterns are set
in `.oxicleanrc.json`:

```json
{
  "entries": ["src/**/*.tsx"],
  "rules": { "bloat": ["error", { "threshold": 300 }] }
}
```

## Example Output

```
⚠ Import bloat detected (threshold: 50 modules)

src/pages/Dashboard.tsx (127 modules)
├──  import './components' (89 modules)
└──  import '@/utils' (45 modules)

📊 Statistics:
  Total unique modules: 234
  ├── App code: 145
  ├── Workspace packages: 23
  └── External packages: 66
```

## How It Works

1. Parses JavaScript/TypeScript files using the fast OXC parser
2. Resolves all imports (static `import`, dynamic `import()`, and `require()`)
3. Builds a dependency graph of reachable modules
4. Reports files exceeding the threshold

**Features:**
- Resolves Node.js modules and TypeScript path mappings from `tsconfig.json`
- Handles static and dynamic imports
- Respects `.gitignore` patterns
- Skips test files

## Library Usage

```rust
use oxiclean_bloat::{Config, run_bloat_check, print_warnings_tree};

let cfg = Config {
    root: Some("./my-project".into()),
    threshold: 100,
    entry_glob: Some("src/**/*.ts".to_string()),
    ..Default::default()
};

let warnings = run_bloat_check(cfg.clone())?;
print_warnings_tree(&warnings, cfg.threshold);
```

## License

MIT
