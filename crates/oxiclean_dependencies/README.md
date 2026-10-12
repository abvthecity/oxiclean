# Unused Dependencies Checker

Finds packages a `package.json` lists that no file imports and no script
runs. It is the equivalent of knip's `dependencies` and `devDependencies`
issue types with its plugins off, as one rule, `dependencies`. Its
`fields` option picks the fields to check: `dependencies`,
`devDependencies`, or both, the default.

The root's `package.json` and each workspace package's are checked.
`peerDependencies` and `optionalDependencies` are never reported.

## Usage

```bash
oxiclean dependencies --root ./my-project
```

### Options

- `--root <PATH>` - Root directory to analyze (default: git root)
- `-c, --config <FILE>` - Config file (default: `.oxicleanrc.json` in the root)
- `--format <text|json|sarif>` - Output format

The fields to check are set per package with `overrides`. Packages that
should never be reported, such as one only a tool's config file names, go
in `ignoreDependencies` in `.oxicleanrc.json`, as in knip:

```json
{
  "ignoreDependencies": ["@types/*", "eslint-config-*"],
  "rules": { "dependencies": ["error", { "fields": ["dependencies"] }] },
  "overrides": [
    {
      "packages": ["@acme/app"],
      "rules": { "dependencies": ["warn", { "fields": ["dependencies", "devDependencies"] }] }
    }
  ]
}
```

## Example Output

```
⚠ Unused dependencies detected

packages/app/package.json
└──  package 'lodash' (never used)
```

## What counts

A package is used when:

- a file the package entries reach imports it, in any form: `import`,
  `import type`, `require`, `import()`, `typeof import()` in a type, or
  `/// <reference types>`. The entries are those of the
  unused export checks (see
  [oxiclean_unused_exports](../oxiclean_unused_exports/README.md#entry-files)),
  config files such as `vite.config.ts` and `.eslintrc.cjs` among them.
  A file's import counts for the nearest
  `package.json` that lists the package, from the file's own package up to
  the root, as Node's resolution finds it;
- a script in the `package.json`, a husky hook or a lint-staged command
  runs one of its binaries, directly or through `npx`, `pnpm exec`,
  `yarn`, `pnpm run` or `bun run` of a name that isn't a script,
  `cross-env`, `env-cmd`, `dotenv`, `concurrently` or `sh -c`, or loads it
  with `--require`, `--import` or `--loader` (or `-r` after `node`,
  `mocha` and the like, and in `NODE_OPTIONS`). A binary belongs to the
  package whose installed `package.json` lists it in `bin`; for a package
  that isn't installed, to the one a short list of known binaries names
  (`tsc` for `typescript`) or else the unscoped package named after it.
  Every package of the nearest `package.json` that provides the binary is
  used, as `playwright` and `@playwright/test` both are;
- a config file beside the `package.json` names it: tsconfig `extends`,
  `compilerOptions.types`, `jsxImportSource` or `plugins`, ESLint
  `extends`, `plugins` or `parser`, Babel presets and plugins, PostCSS
  plugins, Prettier plugins and shared configs, or what Jest's and
  Vitest's configs name, by each tool's naming rules (`airbnb` is
  `eslint-config-airbnb`, Jest's `jsdom` is `jest-environment-jsdom`).
  Setup files these configs name are entries, so what they import is used;
- an installed package that a file imports, a script loads or a config
  names lists it in `peerDependencies`;
- it is `typescript`, or `@types/node`, `@types/bun` or `@types/jest`,
  which declare globals.

`@types/x` (or `@types/scope__x` for `@scope/x`) is used when `x` is
imported, is a peer dependency of a used package, or is named in tsconfig
`types`, unless `x` is installed and ships its own types. Running `x`'s
binary doesn't use `@types/x`. A package listed in both `dependencies` and
`devDependencies` is reported in `devDependencies`.

Where oxiclean differs from knip:

- knip's plugins load tool configs by running them; oxiclean reads them
  without running them (see [Plugins](../../README.md#plugins)): tsconfig,
  ESLint, Babel, PostCSS, Prettier, Jest, Vitest, Expo, Storybook, husky
  and lint-staged configs so far, Next.js and Expo Router routes, stories,
  HTML pages, MDX and stylesheets. A package that only another tool's
  config names, that a config computes, or that a Jest preset's own config
  names is reported unless it is in `ignoreDependencies`;
- an import counts for the nearest `package.json` that lists the package,
  so a workspace package can use what the root lists, where knip reports
  the import as unlisted in the workspace package.

## Unused catalog entries

`oxiclean catalog` reports the entries of pnpm's `catalog` (the default
catalog) and `catalogs` in `pnpm-workspace.yaml` that nothing refers to,
as knip's `catalog` issue type. An entry is used when the root's or a
workspace package's `package.json` lists the package with `catalog:` or
`catalog:default` (the default catalog) or `catalog:<name>`, in any
dependency field, or when an `overrides` entry in `pnpm-workspace.yaml` or
the root `package.json` (`pnpm.overrides`) sets it to one.

```
⚠ Unused catalog entries detected

pnpm-workspace.yaml
├──  catalog entry 'lodash' (default) (never referenced)
└──  catalog entry 'vue' (legacy) (never referenced)
```
