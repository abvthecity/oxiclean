# Import Cycles Checker

Finds circular imports in JavaScript/TypeScript projects: files that import
each other, directly or indirectly, at runtime. It is the equivalent of knip's
`cycles` issue type.

## Usage

```bash
# Analyze current directory (defaults to git root)
oxiclean cycles

# Analyze another project
oxiclean cycles --root ./my-project
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
  "rules": { "cycles": ["warn", { "threshold": 3 }] }
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
- Dynamic `import()` only when `includeDynamic` is set. knip has a setting
  for cycles alone (`cycles.dynamicImports`); `includeDynamic` applies to
  every check.
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

- removing the last import of each reported cycle leaves no cycles,
- reported cycles are short, so each one is easy to read,
- the result depends only on the code, not on traversal order: files are
  taken in path order and each cycle starts at its first file by path.

A cycle is reported on its first file by path that the rule applies to, so
`ignorePatterns` and `overrides` hide a cycle only when they cover every file
in it. knip's `cycles.allow` list has no equivalent yet.

Searching a tangle costs up to O(V·(V + E)) for V files and E imports, as in
pnpm. Tangles in real code are small (the largest in the benchmark targets
has 28 files); a random tangle of 2,000 files and 20,000 imports takes about a
second.

Each finding also gives the size of the tangle (strongly connected
component) its cycle belongs to.

## Library Usage

```rust,no_run
use oxiclean_core::CommonArgs;
use oxiclean_cycles::{Config, FindingKind, run_cycles_check};

let result = run_cycles_check(&Config { common: CommonArgs::with_root("./my-project") })?;
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

## Conformance with knip

`tests/fixtures/knip` is knip's own cycle fixture, copied unchanged with its
ISC license notice. oxiclean reports the same cycles on it as knip's test
expects, with and without dynamic imports.

## License

MIT
