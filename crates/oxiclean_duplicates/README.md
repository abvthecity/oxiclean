# Duplicate Exports Checker

Finds JavaScript/TypeScript modules that export the same thing under more
than one name. It is the equivalent of knip's `duplicates` issue type.

```ts
export const Button = () => null;
export const LegacyButton = Button; // reported: a second name for Button
export default Button;              // reported: a third
```

## Usage

```bash
# Analyze current directory (defaults to git root)
oxiclean duplicates

# Analyze another project
oxiclean duplicates --root ./my-project
```

### Options

- `--root <PATH>` - Root directory to analyze (default: git root)
- `-c, --config <FILE>` - Config file (default: `.oxicleanrc.json` in the root)
- `--format <text|json|sarif>` - Output format

The threshold is the number of names one thing is exported under and
defaults to 2, so every duplicate is reported. It, the entry files and ignore
patterns are set in `.oxicleanrc.json`:

```json
{
  "rules": { "duplicates": "warn" }
}
```

## Example Output

```
⚠ Duplicate exports detected (threshold: 2 names)

src/button.ts
└──  export 'Button' (exported as 3 names)
        src/button.ts:2 also exports it as 'LegacyButton'
        src/button.ts:3 also exports it as 'default'
```

## What counts

As in knip, a name repeats an export when it is declared as:

- `export const b = a`, where `a` is exported by the same file, and that
  export is neither a re-export (`export { a } from './a'`) nor an import
  from another file of the project passed on (`export { a }` or
  `export const a = x` of an imported `x`). An import from a package passed
  on counts as the module's own export;
- `export default a`, where `a` is exported by the same file in any way
  except `export { a }`.

These don't count, as in knip:

- a name whose statement has an `@alias` tag in the nearest block comment
  before it (a `/** */` or `/* */` comment, with only whitespace and `//`
  lines in between);
- renames with `export { a as b }`;
- `export = a`.

Where oxiclean differs from knip:

- the original export may come after the name that repeats it; knip only
  looks back at exports it has already seen;
- a tag must start a line or follow whitespace, so `{@alias}` and
  `me@alias.dev` are not `@alias` tags (knip reads both as one);
- names bound by nested destructuring (`export const { a: { b } } = o`),
  array patterns with defaults, and `export import A = N.B` are exports, so
  `export const c = b` repeats `b`; knip doesn't see those names;
- a `var` declared inside a block and exported at the top level isn't a
  top-level binding, so `export const b = a` of it isn't counted.

Every parsed file outside `node_modules` is checked, which means the entry
files and everything they import.

## Library Usage

```rust,no_run
use oxiclean_core::CommonArgs;
use oxiclean_duplicates::{Config, FindingKind, run_duplicates_check};

let result =
    run_duplicates_check(&Config { common: CommonArgs::with_root("./my-project") })?;
for f in &result.findings {
    if let FindingKind::DuplicateExport(dup) = &f.kind {
        let names: Vec<&str> = dup.names.iter().map(|n| n.name.as_str()).collect();
        println!("{}: {}", f.file.display(), names.join(", "));
    }
}
# Ok::<(), oxiclean_core::Error>(())
```

## Conformance with knip

`tests/fixtures/knip-basic` and `tests/fixtures/knip-alias` are knip's own
export fixtures, copied unchanged with its ISC license notice
(`tests/fixtures/KNIP-LICENSE.md`). oxiclean reports the same duplicates on
them as knip's tests expect.

## License

MIT
