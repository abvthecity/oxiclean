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
//!     "import-bloat": ["warn", { "threshold": 400 }],
//!     "import-depth": "off"
//!   },
//!   "overrides": [
//!     {
//!       // `packages` matches workspace package names and `files` matches
//!       // paths relative to the root; with both, a file must match both.
//!       "packages": ["@acme/legacy-*"],
//!       "rules": { "import-bloat": ["error", { "threshold": 1000 }] }
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
    /// an entry.
    pub entries: Vec<String>,
    /// Also follow dynamic `import()` calls. They are skipped by default
    /// because lazily loaded modules are split into separate chunks.
    pub include_dynamic: bool,
    /// Globs of files, relative to the root, that are never entries and never
    /// reported.
    pub ignore_patterns: Vec<String>,
    /// Settings per rule. A rule that isn't listed runs with its defaults.
    pub rules: Rules,
    /// Settings for some workspace packages or files. Later overrides win.
    pub overrides: Vec<Override>,
}

/// Every rule oxiclean knows. Each is set to a severity, or to
/// `[severity, options]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct Rules {
    /// Imports and entries that pull in too many unique modules.
    /// Default threshold: 300 modules.
    #[serde(rename = "import-bloat")]
    pub import_bloat: Option<RuleSetting>,
    /// Imports with too long a chain of imports below them.
    /// Default threshold: 20 levels.
    #[serde(rename = "import-depth")]
    pub import_depth: Option<RuleSetting>,
}

impl Rules {
    /// The setting for the rule called `name`, if any.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&RuleSetting> {
        match name {
            "import-bloat" => self.import_bloat.as_ref(),
            "import-depth" => self.import_depth.as_ref(),
            _ => None,
        }
    }

    fn all(&self) -> impl Iterator<Item = &RuleSetting> {
        [&self.import_bloat, &self.import_depth].into_iter().flatten()
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
}

impl RuleSetting {
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

    /// The JSON schema of the config file, pretty-printed.
    #[must_use]
    pub fn json_schema() -> String {
        let schema = schemars::schema_for!(ConfigFile);
        let mut text = serde_json::to_string_pretty(&schema).unwrap_or_default();
        text.push('\n');
        text
    }

    fn validate(&self, path: &Path) -> Result<()> {
        let err = |message: String| Error::Config { path: path.to_path_buf(), message };
        let all_rules = self.rules.all().chain(self.overrides.iter().flat_map(|o| o.rules.all()));
        for rule in all_rules {
            file_globs(&rule.to_config().ignore).map_err(err)?;
        }
        file_globs(&self.ignore_patterns).map_err(err)?;
        for o in &self.overrides {
            if o.packages.is_empty() && o.files.is_empty() {
                return Err(err("each entry in `overrides` needs `files` or `packages`".into()));
            }
            file_globs(&o.files).map_err(err)?;
            name_globs(&o.packages).map_err(err)?;
        }
        Ok(())
    }

    /// The settings of `rule`, with `default_threshold` where the file sets
    /// none.
    ///
    /// # Errors
    /// Fails on an invalid glob; [`ConfigFile::parse`] already rejects those.
    pub fn policy(&self, rule: &str, default_threshold: usize) -> Result<RulePolicy> {
        let err = |message: String| Error::Config { path: CONFIG_FILE_NAME.into(), message };
        let mut base = ResolvedRule {
            enabled: true,
            severity: Severity::Error,
            threshold: default_threshold,
            ignored: false,
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

/// The effective settings of a rule for one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedRule {
    pub enabled: bool,
    pub severity: Severity,
    pub threshold: usize,
    /// The file matches an ignore pattern.
    pub ignored: bool,
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
        let enabled_somewhere =
            self.base.enabled || self.overrides.iter().any(|o| o.rule.enabled == Some(true));
        if !enabled_somewhere {
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
        let p = parse("{}").unwrap().policy("import-bloat", 300).unwrap();
        assert_eq!(p.resolve(Path::new("src/a.ts"), None).threshold, 300);
        assert_eq!(p.min_threshold(), Some(300));
    }

    #[test]
    fn unknown_keys_and_rules_are_rejected() {
        let e = parse(r#"{ "treshold": 3 }"#).unwrap_err().to_string();
        assert!(e.contains(".oxicleanrc.json") && e.contains("treshold"), "{e}");
        let e = parse(r#"{ "rules": { "import-blot": "warn" } }"#).unwrap_err().to_string();
        assert!(e.contains("import-blot"), "{e}");
        assert!(parse(r#"{ "rules": { "import-bloat": ["warn", { "treshold": 1 }] } }"#).is_err());
        assert!(parse(r#"{ "rules": { "import-bloat": "maybe" } }"#).is_err());
        assert!(parse(r#"{ "overrides": [{ "rules": {} }] }"#).is_err());
        assert!(parse(r#"{ "ignorePatterns": ["a/["] }"#).is_err());
    }

    #[test]
    fn severity_forms() {
        let cfg = parse(
            r#"{
                // comments and trailing commas are fine
                "$schema": "./configuration_schema.json",
                "rules": { "import-bloat": "allow", "import-depth": ["deny", {}], },
            }"#,
        )
        .unwrap();
        assert!(!cfg.policy("import-bloat", 1).unwrap().base().enabled);
        let depth = cfg.policy("import-depth", 1).unwrap().base();
        assert!(depth.enabled && depth.severity == Severity::Error);
    }

    const CFG: &str = r#"{
        "ignorePatterns": ["**/fixtures/**"],
        "rules": {
            "import-bloat": ["warn", { "threshold": 100, "ignorePatterns": ["scripts/**"] }]
        },
        "overrides": [
            {
                "packages": ["@acme/*"],
                "rules": { "import-bloat": ["error", { "threshold": 500 }] }
            },
            {
                "packages": ["@acme/legacy"],
                "files": ["packages/legacy/src/old/**"],
                "rules": { "import-bloat": "off" }
            }
        ]
    }"#;

    #[test]
    fn overrides_apply_in_order() {
        let p = parse(CFG).unwrap().policy("import-bloat", 200).unwrap();
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
                "rules": { "import-depth": "off" },
                "overrides": [
                    { "packages": ["web"], "rules": { "import-depth": ["error", { "threshold": 4 }] } }
                ]
            }"#,
        )
        .unwrap();
        let p = cfg.policy("import-depth", 10).unwrap();
        assert_eq!(p.min_threshold(), Some(4));
        assert!(!p.resolve(Path::new("a.ts"), None).enabled);
        assert!(p.resolve(Path::new("a.ts"), Some("web")).reports(4));

        let off = parse(r#"{ "rules": { "import-depth": "off" } }"#).unwrap();
        assert_eq!(off.policy("import-depth", 10).unwrap().min_threshold(), None);
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
