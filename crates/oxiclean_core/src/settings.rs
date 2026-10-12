//! The `oxiclean.toml` configuration file, and how its settings apply to each
//! file a rule reports on.
//!
//! ```toml
//! # Files never treated as entries or reported, relative to the root
//! ignore = ["**/fixtures/**"]
//! entry-globs = ["packages/*/src/**/*.ts"]
//! include-dynamic = false
//!
//! [rules.import-bloat]
//! threshold = 300
//! severity = "warn"          # "error" (default) fails the run; "warn" only reports
//!
//! [rules.import-depth]
//! enabled = false
//!
//! # Later overrides win. `packages` matches workspace package names and
//! # `files` matches paths relative to the root; when both are given a file
//! # must match both.
//! [[overrides]]
//! packages = ["@acme/legacy-*"]
//! [overrides.rules.import-bloat]
//! threshold = 1000
//! ignore = ["src/generated/**"]
//! ```

use std::{
    collections::BTreeMap,
    fmt, fs,
    path::{Path, PathBuf},
};

use globset::{Glob, GlobBuilder, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// File name looked up in the project root.
pub const CONFIG_FILE_NAME: &str = "oxiclean.toml";

/// Rules a config file may configure. A `[rules.*]` table naming anything
/// else is rejected, so typos don't silently do nothing.
pub const KNOWN_RULES: &[&str] = &["import-bloat", "import-depth"];

/// How a finding affects the run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Reported, but the run still succeeds.
    #[serde(alias = "warning")]
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

/// The parsed contents of an `oxiclean.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
#[non_exhaustive]
pub struct ConfigFile {
    /// Entry globs, used when none are given on the command line.
    pub entry_globs: Vec<String>,
    /// Follow dynamic `import()` calls. `--include-dynamic` turns it on too.
    pub include_dynamic: Option<bool>,
    /// Globs of files that are never entries and never reported.
    pub ignore: Vec<String>,
    /// Settings per rule, keyed by rule name (e.g. `import-bloat`).
    pub rules: BTreeMap<String, RuleConfig>,
    /// Settings for some packages or files, applied in order.
    pub overrides: Vec<Override>,
}

/// Settings for one rule. Anything left out keeps its previous value.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct RuleConfig {
    pub enabled: Option<bool>,
    pub severity: Option<Severity>,
    pub threshold: Option<usize>,
    /// Globs of files, relative to the root, this rule does not report on.
    pub ignore: Vec<String>,
}

/// Rule settings for the files of some workspace packages or paths.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct Override {
    /// Workspace package names; globs such as `@scope/*` are allowed.
    pub packages: Vec<String>,
    /// Globs of files, relative to the root.
    pub files: Vec<String>,
    pub rules: BTreeMap<String, RuleConfig>,
}

impl ConfigFile {
    /// Parses a config file's text. `path` is only used in error messages.
    ///
    /// # Errors
    /// Fails on invalid TOML, unknown keys or rules, or invalid globs.
    pub fn parse(text: &str, path: &Path) -> Result<Self> {
        let cfg: Self = toml::from_str(text)
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

    /// Loads `explicit` if given, otherwise `oxiclean.toml` in `root` if it
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

    fn validate(&self, path: &Path) -> Result<()> {
        let err = |message: String| Error::Config { path: path.to_path_buf(), message };
        let all_rules = self.rules.iter().chain(self.overrides.iter().flat_map(|o| o.rules.iter()));
        for (name, rule) in all_rules {
            if !KNOWN_RULES.contains(&name.as_str()) {
                return Err(err(format!(
                    "unknown rule '{name}' (known rules: {})",
                    KNOWN_RULES.join(", ")
                )));
            }
            file_globs(&rule.ignore).map_err(&err)?;
        }
        file_globs(&self.ignore).map_err(&err)?;
        for o in &self.overrides {
            if o.packages.is_empty() && o.files.is_empty() {
                return Err(err("an [[overrides]] entry needs `packages` or `files`".into()));
            }
            file_globs(&o.files).map_err(&err)?;
            name_globs(&o.packages).map_err(&err)?;
        }
        Ok(())
    }

    /// The settings of `rule`, with `default_threshold` where the file sets
    /// none. A `cli_threshold` replaces every threshold in the file.
    ///
    /// # Errors
    /// Fails on an invalid glob; [`ConfigFile::parse`] already rejects those.
    pub fn policy(
        &self,
        rule: &str,
        default_threshold: usize,
        cli_threshold: Option<usize>,
    ) -> Result<RulePolicy> {
        let err = |message: String| Error::Config { path: CONFIG_FILE_NAME.into(), message };
        let mut base = ResolvedRule {
            enabled: true,
            severity: Severity::Error,
            threshold: default_threshold,
            ignored: false,
        };
        let mut ignore = self.ignore.clone();
        if let Some(rc) = self.rules.get(rule) {
            base.apply(rc);
            ignore.extend(rc.ignore.iter().cloned());
        }
        let overrides = self
            .overrides
            .iter()
            .filter_map(|o| o.rules.get(rule).map(|rc| (o, rc)))
            .map(|(o, rc)| {
                Ok(CompiledOverride {
                    packages: (!o.packages.is_empty())
                        .then(|| name_globs(&o.packages))
                        .transpose()?,
                    files: (!o.files.is_empty()).then(|| file_globs(&o.files)).transpose()?,
                    ignore: file_globs(&rc.ignore)?,
                    rule: rc.clone(),
                })
            })
            .collect::<Result<Vec<_>, String>>()
            .map_err(err)?;
        Ok(RulePolicy { base, ignore: file_globs(&ignore).map_err(err)?, overrides, cli_threshold })
    }
}

/// The effective settings of a rule for one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedRule {
    pub enabled: bool,
    pub severity: Severity,
    pub threshold: usize,
    /// The file matches an `ignore` glob.
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
    cli_threshold: Option<usize>,
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
            cli_threshold: None,
        }
    }

    /// The settings outside any override.
    #[must_use]
    pub fn base(&self) -> ResolvedRule {
        let mut r = self.base;
        if let Some(t) = self.cli_threshold {
            r.threshold = t;
        }
        r
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
        if let Some(t) = self.cli_threshold {
            r.threshold = t;
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
        if let Some(t) = self.cli_threshold {
            return Some(t);
        }
        // Overrides stack, so the smallest threshold anywhere is a safe bound.
        let thresholds = self.overrides.iter().filter_map(|o| o.rule.threshold);
        Some(thresholds.fold(self.base.threshold, usize::min))
    }
}

/// Globs over paths relative to the root. `*` stops at `/`, like `--entry-glob`.
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
        ConfigFile::parse(text, Path::new("oxiclean.toml"))
    }

    #[test]
    fn empty_file_is_the_defaults() {
        assert_eq!(parse("").unwrap(), ConfigFile::default());
        let p = parse("").unwrap().policy("import-bloat", 200, None).unwrap();
        assert_eq!(p.resolve(Path::new("src/a.ts"), None).threshold, 200);
        assert_eq!(p.min_threshold(), Some(200));
    }

    #[test]
    fn unknown_keys_and_rules_are_rejected() {
        let e = parse("treshold = 3").unwrap_err().to_string();
        assert!(e.contains("oxiclean.toml"), "{e}");
        let e = format!("{:?}", parse("[rules.import-blot]\nthreshold = 3").unwrap_err());
        assert!(e.contains("unknown rule 'import-blot'"), "{e}");
        assert!(parse("[[overrides]]\n[overrides.rules.import-bloat]\nthreshold = 3").is_err());
        assert!(parse("ignore = ['a/[']").is_err());
    }

    #[test]
    fn severity_accepts_warning_alias() {
        let cfg = parse("[rules.import-depth]\nseverity = 'warning'").unwrap();
        assert_eq!(cfg.rules["import-depth"].severity, Some(Severity::Warn));
    }

    const CFG: &str = r#"
        ignore = ["**/fixtures/**"]

        [rules.import-bloat]
        threshold = 100
        severity = "warn"
        ignore = ["scripts/**"]

        [[overrides]]
        packages = ["@acme/*"]
        [overrides.rules.import-bloat]
        threshold = 500
        severity = "error"

        [[overrides]]
        packages = ["@acme/legacy"]
        files = ["packages/legacy/src/old/**"]
        [overrides.rules.import-bloat]
        enabled = false
    "#;

    #[test]
    fn overrides_apply_in_order() {
        let p = parse(CFG).unwrap().policy("import-bloat", 200, None).unwrap();
        let r = |path: &str, pkg: Option<&str>| p.resolve(Path::new(path), pkg);

        let plain = r("apps/web/src/a.ts", None);
        assert_eq!((plain.enabled, plain.severity, plain.threshold), (true, Severity::Warn, 100));
        assert!(r("scripts/build.ts", None).ignored);
        assert!(r("apps/web/fixtures/x.ts", None).ignored);

        let acme = r("packages/ui/src/a.ts", Some("@acme/ui"));
        assert_eq!((acme.severity, acme.threshold), (Severity::Error, 500));

        // Both `packages` and `files` must match.
        assert!(r("packages/legacy/src/old/a.ts", Some("@acme/legacy")).enabled.eq(&false));
        assert!(r("packages/legacy/src/new/a.ts", Some("@acme/legacy")).enabled);
        assert!(r("packages/legacy/src/old/a.ts", Some("@acme/other")).enabled);

        assert_eq!(p.min_threshold(), Some(100));
        assert!(!r("x.ts", None).reports(99));
        assert!(r("x.ts", None).reports(100));
    }

    #[test]
    fn cli_threshold_wins_everywhere() {
        let p = parse(CFG).unwrap().policy("import-bloat", 200, Some(7)).unwrap();
        assert_eq!(p.resolve(Path::new("a.ts"), Some("@acme/ui")).threshold, 7);
        assert_eq!(p.base().threshold, 7);
        assert_eq!(p.min_threshold(), Some(7));
    }

    #[test]
    fn rule_disabled_except_in_one_package() {
        let cfg = parse(
            r#"
            [rules.import-depth]
            enabled = false
            [[overrides]]
            packages = ["web"]
            [overrides.rules.import-depth]
            enabled = true
            threshold = 4
            "#,
        )
        .unwrap();
        let p = cfg.policy("import-depth", 10, None).unwrap();
        assert_eq!(p.min_threshold(), Some(4));
        assert!(!p.resolve(Path::new("a.ts"), None).enabled);
        assert!(p.resolve(Path::new("a.ts"), Some("web")).reports(4));

        let off = parse("[rules.import-depth]\nenabled = false").unwrap();
        assert_eq!(off.policy("import-depth", 10, None).unwrap().min_threshold(), None);
    }
}
