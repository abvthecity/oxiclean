//! The `.oxicleanrc.json` configuration file, and how its settings apply to
//! each file a rule reports on.
//!
//! The format follows oxlint's `.oxlintrc.json`: comments are allowed, a rule
//! is set to a severity or to `[severity, options]`, and `overrides` apply
//! settings to some files.
//!
//! ```jsonc
//! {
//!   "$schema": "https://raw.githubusercontent.com/abvthecity/oxiclean/main/configuration_schema.json",
//!   // Files that are never entries and never reported, relative to the root
//!   "ignorePatterns": ["**/fixtures/**"],
//!   "entries": ["packages/*/src/**/*.ts"],
//!   "rules": {
//!     "bloat": ["warn", { "threshold": 400 }],
//!     "depth": "off"
//!   },
//!   "overrides": [
//!     {
//!       // `packages` matches workspace package names and `files` matches
//!       // paths relative to the root; with both, a file must match both.
//!       "packages": ["@acme/legacy-*"],
//!       "rules": { "bloat": ["error", { "threshold": 1000 }] }
//!     }
//!   ]
//! }
//! ```

use std::{
    fmt, fs,
    path::{Path, PathBuf},
};

use globset::{Glob, GlobBuilder, GlobSet, GlobSetBuilder};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    error::{Error, Result},
    jsonc::strip_jsonc,
};

/// File name looked up in the project root.
pub const CONFIG_FILE_NAME: &str = ".oxicleanrc.json";

/// How a finding affects the run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Reported, but the run still succeeds.
    Warn,
    /// Reported, and the run fails.
    #[default]
    Error,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Warn => "warn",
            Self::Error => "error",
        })
    }
}

/// Configuration for oxiclean, read from `.oxicleanrc.json` in the project
/// root. Comments and trailing commas are allowed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
#[schemars(title = "Oxiclean configuration")]
#[non_exhaustive]
pub struct ConfigFile {
    /// Path or URL of this JSON schema, for editor support.
    #[serde(rename = "$schema")]
    pub schema: Option<String>,
    /// Globs of entry files, relative to the root (e.g.
    /// "apps/*/src/pages/**/*.tsx"). `*` stays within one directory and `**`
    /// crosses directories; a pattern without glob characters matches any path
    /// containing it. When empty, every JS/TS file inside a `src` directory is
    /// an entry; the unused export and member checks start from each
    /// package's entry points instead (package.json `main`, `exports`, `bin` and
    /// `scripts`, `index` files, tests and declaration files).
    pub entries: Vec<String>,
    /// Also follow dynamic `import()` calls. They are skipped by default
    /// because lazily loaded modules are split into separate chunks.
    pub include_dynamic: bool,
    /// Also scan installed packages: parse files inside `node_modules` and
    /// follow their imports, so a dependency counts every module it loads.
    /// Off by default, which counts each imported package file as a single
    /// module and keeps runs fast.
    pub include_node_modules: bool,
    /// Globs of files, relative to the root, that are never entries and never
    /// reported.
    pub ignore_patterns: Vec<String>,
    /// Names of packages, globs such as "@types/*" allowed, that the unused
    /// dependency checks never report, as knip's `ignoreDependencies`.
    pub ignore_dependencies: Vec<String>,
    /// Turns plugins on or off. A plugin that isn't listed is on when its
    /// tool is used: when a package.json lists one of its packages, or
    /// always for file formats and TypeScript.
    pub plugins: PluginSettings,
    /// Settings per rule. A rule that isn't listed runs with its defaults.
    pub rules: Rules,
    /// Settings for some workspace packages or files. Later overrides win.
    pub overrides: Vec<Override>,
    /// Settings for the `trace` command, which reads the output of
    /// `tsc --generateTrace`.
    pub trace: TraceSettings,
}

/// Settings for the `trace` command.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct TraceSettings {
    /// The directory `tsc --generateTrace <dir>` wrote, relative to the
    /// root, read when `oxiclean trace` is given none. oxiclean never runs
    /// `tsc` itself.
    pub dir: Option<String>,
}

/// Plugins: what oxiclean knows about a file format or a tool. `true`
/// turns one on and `false` off; one left out is on when its tool is used.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct PluginSettings {
    /// HTML pages: `<script>` blocks and `<script src>`; pages that load the
    /// project's scripts are entries. Always on by default.
    pub html: Option<bool>,
    /// MDX files: their `import` and `export` lines. Always on by default.
    pub mdx: Option<bool>,
    /// Vue single-file components' `<script>` blocks. Always on by default.
    pub vue: Option<bool>,
    /// Svelte components' `<script>` blocks. Always on by default.
    pub svelte: Option<bool>,
    /// Astro components' frontmatter and `<script>` blocks. Always on by
    /// default.
    pub astro: Option<bool>,
    /// Stylesheets (`.css`, `.scss`, `.sass`, `.less`): `@import`, `@use`,
    /// `@forward` and Tailwind's `@plugin`, `@config` and `@reference`.
    /// Always on by default.
    pub css: Option<bool>,
    /// The packages tsconfig files name in `extends`, `types`,
    /// `jsxImportSource` and `plugins`. Always on by default.
    pub typescript: Option<bool>,
    /// The packages ESLint's `.eslintrc*` files and `eslintConfig` name in
    /// `extends`, `plugins`, `parser` and import resolvers. Always on by
    /// default.
    pub eslint: Option<bool>,
    /// The presets and plugins Babel's config files and `babel` name.
    /// Always on by default.
    pub babel: Option<bool>,
    /// The plugins and syntaxes PostCSS's config files and `postcss` name.
    /// Always on by default.
    pub postcss: Option<bool>,
    /// The shared config and plugins Prettier's config files and
    /// `prettier` name. Always on by default.
    pub prettier: Option<bool>,
    /// What Jest's config names: presets, environments, transforms,
    /// reporters and the like are used, and setup files are entries. On
    /// when a package.json lists `jest`.
    pub jest: Option<bool>,
    /// What the `test` options of Vitest's or Vite's config name:
    /// environments, reporters and coverage providers are used, and setup
    /// files are entries. On when a package.json lists `vitest`.
    pub vitest: Option<bool>,
    /// Next.js pages, App Router files (`page`, `layout`, `route` and the
    /// like), middleware and instrumentation are entries. On when a
    /// package.json lists `next`.
    pub next: Option<bool>,
    /// Expo: config plugins in `app.json` and `app.config.*`, a `main` such
    /// as `expo-router/entry`, and Expo Router's routes in `app`, which are
    /// entries. On when a package.json lists `expo`.
    pub expo: Option<bool>,
    /// Storybook: the addons and framework `.storybook/main.*` names are
    /// used, and stories (`*.stories.*`) and `.storybook`'s `main`,
    /// `preview` and `manager` are entries. On when a package.json lists
    /// `storybook`.
    pub storybook: Option<bool>,
    /// The commands husky's hooks in `.husky` and package.json
    /// `simple-git-hooks` run, read as scripts. On when a package.json
    /// lists `husky` or `simple-git-hooks`.
    pub husky: Option<bool>,
    /// The commands lint-staged's config runs, read as scripts. On when a
    /// package.json lists `lint-staged`.
    #[serde(rename = "lint-staged")]
    pub lint_staged: Option<bool>,
}

impl PluginSettings {
    /// The setting for the plugin called `name`, if any.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<bool> {
        match name {
            "html" => self.html,
            "mdx" => self.mdx,
            "vue" => self.vue,
            "svelte" => self.svelte,
            "astro" => self.astro,
            "css" => self.css,
            "typescript" => self.typescript,
            "eslint" => self.eslint,
            "babel" => self.babel,
            "postcss" => self.postcss,
            "prettier" => self.prettier,
            "jest" => self.jest,
            "vitest" => self.vitest,
            "next" => self.next,
            "expo" => self.expo,
            "storybook" => self.storybook,
            "husky" => self.husky,
            "lint-staged" => self.lint_staged,
            _ => None,
        }
    }

    /// Every plugin set to `on`.
    #[must_use]
    pub fn all(on: bool) -> Self {
        let v = Some(on);
        Self {
            html: v,
            mdx: v,
            vue: v,
            svelte: v,
            astro: v,
            css: v,
            typescript: v,
            eslint: v,
            babel: v,
            postcss: v,
            prettier: v,
            jest: v,
            vitest: v,
            next: v,
            expo: v,
            storybook: v,
            husky: v,
            lint_staged: v,
        }
    }
}

/// Every rule oxiclean knows. Each is set to a severity, or to
/// `[severity, options]`. A rule the file doesn't set is on, at `error`,
/// except `heavy-dependencies`, `hot-spots` and `duplicate-packages`,
/// which are at `warn`. Each command runs a
/// group of rules, and runs only the ones that are on.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
#[non_exhaustive]
pub struct Rules {
    /// Imports and entries that pull in too many unique modules.
    /// Default threshold: 300 modules.
    pub bloat: Option<RuleSetting>,
    /// Imports with too long a chain of imports below them.
    /// Default threshold: 20 levels.
    pub depth: Option<RuleSetting>,
    /// Files that import each other, directly or indirectly, at runtime.
    /// Type-only imports don't count. Default threshold: 1 file, so every
    /// cycle is reported.
    pub cycles: Option<RuleSetting>,
    /// Files that export the same thing under more than one name, as knip's
    /// `duplicates`. Exports tagged `@alias` don't count. Default threshold:
    /// 2 names.
    pub duplicates: Option<RuleSetting>,
    /// Exports that no other file imports, as knip's `exports`. Entry files'
    /// exports and exports tagged `@public`, `@beta` or `@alias` don't
    /// count. Default threshold: 1.
    pub unused_exports: Option<RuleSetting>,
    /// Exported types, interfaces and enums that no other file imports, as
    /// knip's `types`. The same exceptions apply. Default threshold: 1.
    pub unused_types: Option<RuleSetting>,
    /// Members of used enums that nothing uses, as knip's `enumMembers`.
    /// Members tagged `@public`, `@beta` or `@alias` don't count. Default
    /// threshold: 1.
    pub unused_enum_members: Option<RuleSetting>,
    /// What used namespaces export that nothing uses, as knip's
    /// `namespaceMembers`. The same exceptions apply. Default threshold: 1.
    pub unused_namespace_members: Option<RuleSetting>,
    /// Packages a package.json lists in `dependencies` or `devDependencies`
    /// that no file imports and no script runs, as knip's `dependencies` and
    /// `devDependencies`. The `fields` option picks which of the two to
    /// check. A package in both is reported in `devDependencies`. Default
    /// threshold: 1.
    pub dependencies: Option<RuleSetting>,
    /// Entries of pnpm's `catalog` and `catalogs` in pnpm-workspace.yaml
    /// that no package.json refers to with `catalog:`, as knip's `catalog`.
    /// Default threshold: 1.
    pub catalog: Option<RuleSetting>,
    /// Barrel files, modules that mostly re-export others, that pull in
    /// too many modules when imported. Default threshold: 25 modules.
    pub barrels: Option<RuleSetting>,
    /// Third-party packages that pull in too many modules, counted through
    /// `node_modules`, reported on the package.json that lists them.
    /// Default threshold: 100 modules. Default severity: `warn`, as a heavy
    /// package can be worth its weight.
    pub heavy_dependencies: Option<RuleSetting>,
    /// Packages `package-lock.json` or `pnpm-lock.yaml` installs at more
    /// than one version. Default threshold: 2 versions.
    pub duplicate_versions: Option<RuleSetting>,
    /// Files a tsconfig program type-checks that nobody needs checked:
    /// build output in a tsconfig's `outDir` or `declarationDir`, and
    /// minified or very large JavaScript that `allowJs` pulls in. Default
    /// threshold: 1 file.
    pub program_files: Option<RuleSetting>,
    /// Files that more than one tsconfig program type-checks, from configs
    /// in different directories where neither extends the other. Default
    /// threshold: 1 file.
    pub overlap: Option<RuleSetting>,
    /// Project references that don't match what packages import: missing,
    /// stale, cyclic or pointing at configs that aren't `composite` or don't
    /// exist; and, in repos without references, package source that two or
    /// more other packages type-check. Default threshold: 1.
    pub references: Option<RuleSetting>,
    /// Compiler options that slow every `tsc` run: `skipLibCheck` unset,
    /// configs scripts run with `tsc` that aren't `incremental`, and debug
    /// options such as `extendedDiagnostics` left on. Default threshold: 1.
    pub tsconfig: Option<RuleSetting>,
    /// Programs without `compilerOptions.types` that load `@types` packages
    /// neither their package.json nor the root one declares. Default
    /// threshold: 1 package.
    pub ambient_types: Option<RuleSetting>,
    /// Programs that emit declarations and already have, or are a few
    /// annotations from, the explicit types `isolatedDeclarations` needs;
    /// and Deno packages with `exports` whose public API has JSR slow
    /// types. Has no threshold.
    pub isolated_declarations: Option<RuleSetting>,
    /// Object and array literals, and `.json` files programs import, with
    /// so many values that inferring their types slows `tsc`. Default
    /// threshold: 10000 values.
    pub large_literals: Option<RuleSetting>,
    /// Package imports whose declaration files, beyond the `@types`
    /// packages a program loads anyway, run to so many lines that parsing
    /// and binding them slows `tsc`. Default threshold: 100000 lines.
    pub type_bloat: Option<RuleSetting>,
    /// Types that expand into huge numbers of types: template literal and
    /// union intersection products, recursive key paths, tuple arithmetic
    /// and long builder chains. Default threshold: 1000 types.
    pub complex_types: Option<RuleSetting>,
    /// Spans of a `tsc --generateTrace` trace where checking took long, as
    /// `@typescript/analyze-trace` reports them, joined to what
    /// `complex-types` flags. The threshold is analyze-trace's
    /// `forceMillis`, and the `skipMillis` option its `skipMillis`.
    /// Default threshold: 500 ms. Default severity: `warn`; a hot spot on
    /// a type `complex-types` flags is an `error`.
    pub hot_spots: Option<RuleSetting>,
    /// Packages a trace shows `tsc` loading from more than one directory.
    /// Default threshold: 2 directories. Default severity: `warn`.
    pub duplicate_packages: Option<RuleSetting>,
}

impl Rules {
    /// The setting for the rule called `name`, if any.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&RuleSetting> {
        match name {
            "bloat" => self.bloat.as_ref(),
            "depth" => self.depth.as_ref(),
            "cycles" => self.cycles.as_ref(),
            "duplicates" => self.duplicates.as_ref(),
            "unused-exports" => self.unused_exports.as_ref(),
            "unused-types" => self.unused_types.as_ref(),
            "unused-enum-members" => self.unused_enum_members.as_ref(),
            "unused-namespace-members" => self.unused_namespace_members.as_ref(),
            "dependencies" => self.dependencies.as_ref(),
            "catalog" => self.catalog.as_ref(),
            "barrels" => self.barrels.as_ref(),
            "heavy-dependencies" => self.heavy_dependencies.as_ref(),
            "duplicate-versions" => self.duplicate_versions.as_ref(),
            "program-files" => self.program_files.as_ref(),
            "overlap" => self.overlap.as_ref(),
            "references" => self.references.as_ref(),
            "tsconfig" => self.tsconfig.as_ref(),
            "ambient-types" => self.ambient_types.as_ref(),
            "isolated-declarations" => self.isolated_declarations.as_ref(),
            "large-literals" => self.large_literals.as_ref(),
            "type-bloat" => self.type_bloat.as_ref(),
            "complex-types" => self.complex_types.as_ref(),
            "hot-spots" => self.hot_spots.as_ref(),
            "duplicate-packages" => self.duplicate_packages.as_ref(),
            _ => None,
        }
    }

    fn all(&self) -> impl Iterator<Item = &RuleSetting> {
        [
            &self.bloat,
            &self.depth,
            &self.cycles,
            &self.duplicates,
            &self.unused_exports,
            &self.unused_types,
            &self.unused_enum_members,
            &self.unused_namespace_members,
            &self.dependencies,
            &self.catalog,
            &self.barrels,
            &self.heavy_dependencies,
            &self.duplicate_versions,
            &self.program_files,
            &self.overlap,
            &self.references,
            &self.tsconfig,
            &self.ambient_types,
            &self.isolated_declarations,
            &self.large_literals,
            &self.type_bloat,
            &self.complex_types,
            &self.hot_spots,
            &self.duplicate_packages,
        ]
        .into_iter()
        .flatten()
    }
}

/// A rule's severity, optionally with options.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(
    untagged,
    expecting = "a severity (\"off\", \"warn\" or \"error\") or [severity, options] with known options"
)]
pub enum RuleSetting {
    Severity(AllowWarnDeny),
    WithOptions(AllowWarnDeny, RuleOptions),
}

/// `off` (or `allow`) turns a rule off, `warn` reports without failing the
/// run, and `error` (or `deny`) fails it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum AllowWarnDeny {
    Off,
    Allow,
    Warn,
    Error,
    Deny,
}

/// Options shared by every rule.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
#[non_exhaustive]
pub struct RuleOptions {
    /// Report values at or above this.
    pub threshold: Option<usize>,
    /// Globs of files, relative to the root, this rule does not report on.
    pub ignore_patterns: Vec<String>,
    /// For `dependencies`: the package.json fields to check. Both by
    /// default.
    pub fields: Option<Vec<DependencyField>>,
    /// For `hot-spots`: spans of the trace shorter than this many
    /// milliseconds are never reported, as analyze-trace's `skipMillis`.
    /// Default: 100.
    pub skip_millis: Option<usize>,
}

/// A package.json field `dependencies` can check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum DependencyField {
    Dependencies,
    DevDependencies,
}

/// Rule settings for the files of some workspace packages or paths.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct Override {
    /// Globs of files, relative to the root.
    pub files: Vec<String>,
    /// Workspace package names; globs such as "@scope/*" are allowed.
    pub packages: Vec<String>,
    pub rules: Rules,
}

/// One rule's settings in a form that stacks: anything left out keeps its
/// previous value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct RuleConfig {
    enabled: Option<bool>,
    severity: Option<Severity>,
    threshold: Option<usize>,
    ignore: Vec<String>,
    fields: Option<DependencyFields>,
}

impl RuleSetting {
    /// Whether the setting turns the rule on, read from its level alone.
    fn on(&self) -> bool {
        let (Self::Severity(level) | Self::WithOptions(level, _)) = self;
        !matches!(level, AllowWarnDeny::Off | AllowWarnDeny::Allow)
    }

    fn to_config(&self) -> RuleConfig {
        let (level, options) = match self {
            Self::Severity(level) => (*level, None),
            Self::WithOptions(level, options) => (*level, Some(options)),
        };
        let severity = match level {
            AllowWarnDeny::Off | AllowWarnDeny::Allow => None,
            AllowWarnDeny::Warn => Some(Severity::Warn),
            AllowWarnDeny::Error | AllowWarnDeny::Deny => Some(Severity::Error),
        };
        RuleConfig {
            enabled: Some(severity.is_some()),
            severity,
            threshold: options.and_then(|o| o.threshold),
            ignore: options.map(|o| o.ignore_patterns.clone()).unwrap_or_default(),
            fields: options.and_then(|o| o.fields.as_deref()).map(DependencyFields::from),
        }
    }
}

impl ConfigFile {
    /// Parses a config file's text. `path` is only used in error messages.
    ///
    /// # Errors
    /// Fails on invalid JSON, unknown keys or rules, or invalid globs.
    pub fn parse(text: &str, path: &Path) -> Result<Self> {
        let cfg: Self = serde_json::from_str(&strip_jsonc(text))
            .map_err(|e| Error::Config { path: path.to_path_buf(), message: e.to_string() })?;
        cfg.validate(path)?;
        Ok(cfg)
    }

    /// Reads `path`.
    ///
    /// # Errors
    /// Fails if the file can't be read or is invalid.
    pub fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .map_err(|e| Error::Config { path: path.to_path_buf(), message: e.to_string() })?;
        Self::parse(&text, path)
    }

    /// Loads `explicit` if given, otherwise `.oxicleanrc.json` in `root` if it
    /// exists. Returns the defaults, and no path, when there is no file.
    ///
    /// # Errors
    /// Fails if the file can't be read or is invalid.
    pub fn discover(root: &Path, explicit: Option<&Path>) -> Result<(Self, Option<PathBuf>)> {
        let path = match explicit {
            Some(p) => p.to_path_buf(),
            None => {
                let p = root.join(CONFIG_FILE_NAME);
                if !p.is_file() {
                    return Ok((Self::default(), None));
                }
                p
            }
        };
        Ok((Self::load(&path)?, Some(path)))
    }

    /// The options `rule` is set with outside the overrides, if any.
    #[must_use]
    pub fn rule_options(&self, rule: &str) -> Option<&RuleOptions> {
        match self.rules.get(rule)? {
            RuleSetting::WithOptions(_, options) => Some(options),
            RuleSetting::Severity(_) => None,
        }
    }

    /// The JSON schema of the config file, pretty-printed.
    #[must_use]
    pub fn json_schema() -> String {
        let schema = schemars::schema_for!(ConfigFile);
        let mut text = serde_json::to_string_pretty(&schema).unwrap_or_default();
        text.push('\n');
        text
    }

    /// The packages the unused dependency checks never report, from
    /// `ignoreDependencies`.
    ///
    /// # Errors
    /// Fails on an invalid glob; [`ConfigFile::parse`] already rejects those.
    pub fn ignored_dependencies(&self) -> Result<GlobSet> {
        name_globs(&self.ignore_dependencies)
            .map_err(|message| Error::Config { path: PathBuf::new(), message })
    }

    fn validate(&self, path: &Path) -> Result<()> {
        let err = |message: String| Error::Config { path: path.to_path_buf(), message };
        let all_rules = self.rules.all().chain(self.overrides.iter().flat_map(|o| o.rules.all()));
        for rule in all_rules {
            file_globs(&rule.to_config().ignore).map_err(err)?;
        }
        file_globs(&self.ignore_patterns).map_err(err)?;
        name_globs(&self.ignore_dependencies).map_err(err)?;
        for o in &self.overrides {
            if o.packages.is_empty() && o.files.is_empty() {
                return Err(err("each entry in `overrides` needs `files` or `packages`".into()));
            }
            file_globs(&o.files).map_err(err)?;
            name_globs(&o.packages).map_err(err)?;
        }
        Ok(())
    }

    /// Whether `rule` is on for any file: on outside the overrides, or
    /// turned on by one. A command skips the rules that aren't.
    #[must_use]
    pub fn enabled(&self, rule: &str) -> bool {
        enabled_anywhere(
            self.rules.get(rule).is_none_or(RuleSetting::on),
            self.overrides.iter().map(|o| o.rules.get(rule).map(RuleSetting::on)),
        )
    }

    /// The settings of `rule`, with `default_threshold` and
    /// `default_severity` where the file sets none. Each rule passes its
    /// own defaults: most are `error`, while `heavy-dependencies`,
    /// `hot-spots` and `duplicate-packages` are `warn`.
    ///
    /// # Errors
    /// Fails on an invalid glob; [`ConfigFile::parse`] already rejects those.
    pub fn policy(
        &self,
        rule: &str,
        default_threshold: usize,
        default_severity: Severity,
    ) -> Result<RulePolicy> {
        let err = |message: String| Error::Config { path: CONFIG_FILE_NAME.into(), message };
        let mut base = ResolvedRule {
            enabled: true,
            severity: default_severity,
            threshold: default_threshold,
            ignored: false,
            fields: DependencyFields::default(),
        };
        let mut ignore = self.ignore_patterns.clone();
        if let Some(setting) = self.rules.get(rule) {
            let rc = setting.to_config();
            base.apply(&rc);
            ignore.extend(rc.ignore);
        }
        let overrides = self
            .overrides
            .iter()
            .filter_map(|o| o.rules.get(rule).map(|s| (o, s.to_config())))
            .map(|(o, rc)| {
                Ok(CompiledOverride {
                    packages: (!o.packages.is_empty())
                        .then(|| name_globs(&o.packages))
                        .transpose()?,
                    files: (!o.files.is_empty()).then(|| file_globs(&o.files)).transpose()?,
                    ignore: file_globs(&rc.ignore)?,
                    rule: rc,
                })
            })
            .collect::<Result<Vec<_>, String>>()
            .map_err(err)?;
        Ok(RulePolicy { base, ignore: file_globs(&ignore).map_err(err)?, overrides })
    }
}

/// Whether a rule is on for any file: on outside the overrides (`base`),
/// or turned on by one of them. Each override says whether it turns the
/// rule on or off, or `None` when it doesn't set the rule.
fn enabled_anywhere(base: bool, mut overrides: impl Iterator<Item = Option<bool>>) -> bool {
    base || overrides.any(|o| o == Some(true))
}

/// The effective settings of a rule for one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedRule {
    pub enabled: bool,
    pub severity: Severity,
    pub threshold: usize,
    /// The file matches an ignore pattern.
    pub ignored: bool,
    /// The package.json fields `dependencies` checks.
    pub fields: DependencyFields,
}

/// Which package.json fields `dependencies` checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct DependencyFields {
    pub dependencies: bool,
    pub dev_dependencies: bool,
}

impl Default for DependencyFields {
    fn default() -> Self {
        Self { dependencies: true, dev_dependencies: true }
    }
}

impl From<&[DependencyField]> for DependencyFields {
    fn from(fields: &[DependencyField]) -> Self {
        Self {
            dependencies: fields.contains(&DependencyField::Dependencies),
            dev_dependencies: fields.contains(&DependencyField::DevDependencies),
        }
    }
}

impl DependencyFields {
    /// Whether `field` is checked.
    #[must_use]
    pub fn includes(self, field: DependencyField) -> bool {
        match field {
            DependencyField::Dependencies => self.dependencies,
            DependencyField::DevDependencies => self.dev_dependencies,
        }
    }
}

impl ResolvedRule {
    fn apply(&mut self, rc: &RuleConfig) {
        if let Some(e) = rc.enabled {
            self.enabled = e;
        }
        if let Some(s) = rc.severity {
            self.severity = s;
        }
        if let Some(t) = rc.threshold {
            self.threshold = t;
        }
        if let Some(f) = rc.fields {
            self.fields = f;
        }
    }

    /// Whether a finding with `value` on this file is reported.
    #[must_use]
    pub fn reports(&self, value: usize) -> bool {
        self.enabled && !self.ignored && value >= self.threshold
    }
}

#[derive(Debug, Clone)]
struct CompiledOverride {
    packages: Option<GlobSet>,
    files: Option<GlobSet>,
    ignore: GlobSet,
    rule: RuleConfig,
}

impl CompiledOverride {
    fn matches(&self, rel: &Path, package: Option<&str>) -> bool {
        let package_ok =
            self.packages.as_ref().is_none_or(|g| package.is_some_and(|p| g.is_match(p)));
        let files_ok = self.files.as_ref().is_none_or(|g| g.is_match(rel));
        package_ok && files_ok
    }
}

/// A rule's settings from the config file, ready to look up per file.
#[derive(Debug, Clone)]
pub struct RulePolicy {
    base: ResolvedRule,
    ignore: GlobSet,
    overrides: Vec<CompiledOverride>,
}

impl RulePolicy {
    /// A rule that is enabled everywhere with one threshold, as when there is
    /// no config file.
    #[must_use]
    pub fn fixed(threshold: usize) -> Self {
        Self {
            base: ResolvedRule {
                enabled: true,
                severity: Severity::Error,
                threshold,
                ignored: false,
                fields: DependencyFields::default(),
            },
            ignore: GlobSet::empty(),
            overrides: Vec::new(),
        }
    }

    /// The settings outside any override.
    #[must_use]
    pub fn base(&self) -> ResolvedRule {
        self.base
    }

    /// The settings for `rel` (a path relative to the root) in workspace
    /// package `package`.
    #[must_use]
    pub fn resolve(&self, rel: &Path, package: Option<&str>) -> ResolvedRule {
        let mut r = self.base;
        r.ignored = self.ignore.is_match(rel);
        for o in self.overrides.iter().filter(|o| o.matches(rel, package)) {
            r.apply(&o.rule);
            r.ignored |= o.ignore.is_match(rel);
        }
        r
    }

    /// The lowest threshold any enabled file can have, so a checker can
    /// collect every candidate in one pass before [`Self::resolve`] filters.
    /// `None` when the rule is disabled everywhere.
    #[must_use]
    pub fn min_threshold(&self) -> Option<usize> {
        if !enabled_anywhere(self.base.enabled, self.overrides.iter().map(|o| o.rule.enabled)) {
            return None;
        }
        // Overrides stack, so the smallest threshold anywhere is a safe bound.
        let thresholds = self.overrides.iter().filter_map(|o| o.rule.threshold);
        Some(thresholds.fold(self.base.threshold, usize::min))
    }
}

/// Globs over paths relative to the root. `*` stops at `/`, like `entries`.
pub(crate) fn file_globs(patterns: &[String]) -> Result<GlobSet, String> {
    build_globs(patterns, true)
}

/// Globs over package names, where `*` also matches `/` (`@scope/*`).
pub(crate) fn name_globs(patterns: &[String]) -> Result<GlobSet, String> {
    build_globs(patterns, false)
}

fn build_globs(patterns: &[String], literal_separator: bool) -> Result<GlobSet, String> {
    let mut b = GlobSetBuilder::new();
    for p in patterns {
        let glob: Glob = GlobBuilder::new(p.trim_start_matches("./"))
            .literal_separator(literal_separator)
            .build()
            .map_err(|e| format!("invalid glob '{p}': {e}"))?;
        b.add(glob);
    }
    b.build().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<ConfigFile> {
        ConfigFile::parse(text, Path::new(".oxicleanrc.json"))
    }

    #[test]
    fn empty_file_is_the_defaults() {
        assert_eq!(parse("{}").unwrap(), ConfigFile::default());
        let p = parse("{}").unwrap().policy("bloat", 300, Severity::Error).unwrap();
        assert_eq!(p.resolve(Path::new("src/a.ts"), None).threshold, 300);
        assert_eq!(p.min_threshold(), Some(300));
    }

    #[test]
    fn unknown_keys_and_rules_are_rejected() {
        let e = parse(r#"{ "treshold": 3 }"#).unwrap_err().to_string();
        assert!(e.contains(".oxicleanrc.json") && e.contains("treshold"), "{e}");
        let e = parse(r#"{ "rules": { "import-blot": "warn" } }"#).unwrap_err().to_string();
        assert!(e.contains("import-blot"), "{e}");
        assert!(parse(r#"{ "rules": { "bloat": ["warn", { "treshold": 1 }] } }"#).is_err());
        assert!(parse(r#"{ "rules": { "bloat": "maybe" } }"#).is_err());
        assert!(parse(r#"{ "overrides": [{ "rules": {} }] }"#).is_err());
        assert!(parse(r#"{ "ignorePatterns": ["a/["] }"#).is_err());
    }

    #[test]
    fn severity_forms() {
        let cfg = parse(
            r#"{
                // comments and trailing commas are fine
                "$schema": "./configuration_schema.json",
                "rules": { "bloat": "allow", "depth": ["deny", {}], },
            }"#,
        )
        .unwrap();
        assert!(!cfg.policy("bloat", 1, Severity::Error).unwrap().base().enabled);
        let depth = cfg.policy("depth", 1, Severity::Error).unwrap().base();
        assert!(depth.enabled && depth.severity == Severity::Error);
    }

    #[test]
    fn default_severities() {
        // A rule the file doesn't set has the severity the rule passes.
        let none = parse("{}").unwrap();
        let heavy = none.policy("heavy-dependencies", 1, Severity::Warn).unwrap();
        assert_eq!(heavy.base().severity, Severity::Warn);
        let unused = none.policy("dependencies", 1, Severity::Error).unwrap();
        assert_eq!(unused.base().severity, Severity::Error);

        // The file's severity wins, outside and inside overrides.
        let cfg = parse(
            r#"{
                "rules": { "heavy-dependencies": "error", "dependencies": ["warn", {}] },
                "overrides": [{ "files": ["legacy/**"], "rules": { "heavy-dependencies": "warn" } }]
            }"#,
        )
        .unwrap();
        let heavy = cfg.policy("heavy-dependencies", 1, Severity::Warn).unwrap();
        assert_eq!(heavy.base().severity, Severity::Error);
        assert_eq!(heavy.resolve(Path::new("legacy/package.json"), None).severity, Severity::Warn);
        assert_eq!(
            cfg.policy("dependencies", 1, Severity::Error).unwrap().base().severity,
            Severity::Warn
        );
    }

    #[test]
    fn enabled_rules() {
        let cfg = parse(
            r#"{
                "rules": { "bloat": "off", "depth": "off", "cycles": "warn" },
                "overrides": [{ "files": ["a/**"], "rules": { "depth": "error" } }]
            }"#,
        )
        .unwrap();
        assert!(!cfg.enabled("bloat"));
        assert!(cfg.enabled("depth"), "an override turns it on");
        assert!(cfg.enabled("cycles"));
        assert!(cfg.enabled("barrels"), "rules the file doesn't set are on");
    }

    #[test]
    fn enabled_rules_with_overrides_that_turn_them_off() {
        let cfg = parse(
            r#"{
                "rules": { "bloat": "off", "depth": "warn" },
                "overrides": [
                    { "files": ["a/**"], "rules": { "bloat": "off", "depth": "off" } },
                    { "files": ["b/**"], "rules": { "depth": ["off", { "threshold": 3 }] } }
                ]
            }"#,
        )
        .unwrap();
        assert!(!cfg.enabled("bloat"), "an override that turns it off doesn't turn it on");
        assert!(cfg.enabled("depth"), "on outside the overrides that turn it off");
        assert!(cfg.policy("depth", 1, Severity::Error).unwrap().min_threshold().is_some());
        assert!(cfg.policy("bloat", 1, Severity::Error).unwrap().min_threshold().is_none());
    }

    const CFG: &str = r#"{
        "ignorePatterns": ["**/fixtures/**"],
        "rules": {
            "bloat": ["warn", { "threshold": 100, "ignorePatterns": ["scripts/**"] }]
        },
        "overrides": [
            {
                "packages": ["@acme/*"],
                "rules": { "bloat": ["error", { "threshold": 500 }] }
            },
            {
                "packages": ["@acme/legacy"],
                "files": ["packages/legacy/src/old/**"],
                "rules": { "bloat": "off" }
            }
        ]
    }"#;

    #[test]
    fn overrides_apply_in_order() {
        let p = parse(CFG).unwrap().policy("bloat", 200, Severity::Error).unwrap();
        let r = |path: &str, pkg: Option<&str>| p.resolve(Path::new(path), pkg);

        let plain = r("apps/web/src/a.ts", None);
        assert_eq!((plain.enabled, plain.severity, plain.threshold), (true, Severity::Warn, 100));
        assert!(r("scripts/build.ts", None).ignored);
        assert!(r("apps/web/fixtures/x.ts", None).ignored);

        let acme = r("packages/ui/src/a.ts", Some("@acme/ui"));
        assert_eq!((acme.severity, acme.threshold), (Severity::Error, 500));

        // Both `packages` and `files` must match.
        assert!(!r("packages/legacy/src/old/a.ts", Some("@acme/legacy")).enabled);
        assert!(r("packages/legacy/src/new/a.ts", Some("@acme/legacy")).enabled);
        assert!(r("packages/legacy/src/old/a.ts", Some("@acme/other")).enabled);

        assert_eq!(p.min_threshold(), Some(100));
        assert!(!r("x.ts", None).reports(99));
        assert!(r("x.ts", None).reports(100));
    }

    #[test]
    fn rule_off_except_in_one_package() {
        let cfg = parse(
            r#"{
                "rules": { "depth": "off" },
                "overrides": [
                    { "packages": ["web"], "rules": { "depth": ["error", { "threshold": 4 }] } }
                ]
            }"#,
        )
        .unwrap();
        let p = cfg.policy("depth", 10, Severity::Error).unwrap();
        assert_eq!(p.min_threshold(), Some(4));
        assert!(!p.resolve(Path::new("a.ts"), None).enabled);
        assert!(p.resolve(Path::new("a.ts"), Some("web")).reports(4));

        let off = parse(r#"{ "rules": { "depth": "off" } }"#).unwrap();
        assert_eq!(off.policy("depth", 10, Severity::Error).unwrap().min_threshold(), None);
    }

    /// The committed schema must match the config types. Run with
    /// `UPDATE_SCHEMA=1` to regenerate it.
    #[test]
    fn committed_schema_is_up_to_date() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../configuration_schema.json");
        let expected = ConfigFile::json_schema();
        if std::env::var_os("UPDATE_SCHEMA").is_some() {
            fs::write(&path, &expected).unwrap();
        }
        let committed = fs::read_to_string(&path).unwrap_or_default().replace("\r\n", "\n");
        assert!(
            committed == expected,
            "configuration_schema.json is out of date; run \
             `UPDATE_SCHEMA=1 cargo test -p oxiclean_core committed_schema`"
        );
    }
}
