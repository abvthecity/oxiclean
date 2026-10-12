# Import Cycles Checker

Finds circular imports in JavaScript/TypeScript projects: files that import
each other, directly or indirectly, at runtime. It is the equivalent of knip's
`cycles` issue type.

## Usage

```bash
# Analyze current directory (defaults to git root)
oxiclean import-cycles

# Analyze another project
oxiclean import-cycles --root ./my-project
```

### Options

- `--root <PATH>` - Root directory to analyze (default: git root)
- `-c, --config <FILE>` - Config file (default: `.oxicleanrc.json` in the root)
- `--format <text|json|sarif>` - Output format

The threshold is the number of files in a cycle and defaults to 1, so every
cycle is reported. It, the entry files and ignore patterns are set in
`.oxicleanrc.json`:

```json
{
  "rules": { "import-cycles": ["warn", { "threshold": 3 }] }
}
```

## Example Output

```
⚠ Circular imports detected (threshold: 1 files)

src/store/index.ts
└──  import './slices' (cycle of 3 files)
        src/store/index.ts:4 imports './slices'
        src/store/slices.ts:2 imports './user'
        src/store/user.ts:1 imports './index'
        part of a tangle of 7 files that import each other
```

## What counts

- Static imports, re-exports and `require()` calls.
- Not type-only imports (`import type`, or `import { type A }` when every
  name is a type): TypeScript erases them, so they can't cause a cycle at
  runtime.
- Dynamic `import()` only when `includeDynamic` is set, as in knip.
- Not files inside `node_modules`.

## How cycles are chosen

A tangle of files that import each other usually contains many overlapping
cycles, and listing all of them is neither practical nor useful. The checker
uses a port of pnpm's
[graph-sequencer](https://github.com/pnpm/pnpm/tree/main/pnpm11/deps/graph-sequencer)
(`oxiclean_core::sequence`), which orders a dependency graph and, where
cycles block the order, picks the longest of the shortest cycles through each
file it searches from. The import that closes each reported cycle is then set
aside and the search repeats until no cycle is left. As a result:

- every circular import runs through the closing import of at least one
  reported cycle,
- reported cycles are short, so each one is easy to read,
- the result depends only on the code, not on traversal order: files are
  taken in path order and each cycle starts at its first file by path.

Each finding also gives the size of the tangle (strongly connected
component) its cycle belongs to.

## Library Usage

```rust,no_run
use oxiclean_core::CommonArgs;
use oxiclean_import_cycles::{Config, FindingKind, run_import_cycles_check};

let result = run_import_cycles_check(&Config { common: CommonArgs::with_root("./my-project") })?;
let cycles: Vec<Vec<String>> = result
    .findings
    .iter()
    .filter_map(|f| match &f.kind {
        FindingKind::Cycle(cycle) => Some(
            cycle.steps.iter().map(|s| format!("{}:{}", s.file.display(), s.line)).collect(),
        ),
        _ => None,
    })
    .collect();
# Ok::<(), oxiclean_core::Error>(())
```

## License

MIT
