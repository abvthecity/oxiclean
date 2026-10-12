use clap::Parser;
use oxiclean_core::{CommonArgs, report::ReportStyle};

/// Options for the unused dependency checks.
#[derive(Debug, Clone, Parser)]
#[command(about = "Check for packages a package.json lists that nothing imports or runs")]
pub struct Config {
    #[command(flatten)]
    pub common: CommonArgs,
}

/// Name of the unused dependencies rule in `.oxicleanrc.json` and in JSON
/// and SARIF output.
pub const DEPENDENCIES_RULE: &str = "dependencies";

/// Threshold used when the config file sets none. Every finding has the
/// value 1, so every unused package is reported.
pub const DEFAULT_THRESHOLD: usize = 1;

/// How unused dependency findings are worded in the terminal report.
pub const DEPENDENCIES_STYLE: ReportStyle = ReportStyle {
    description: "Packages a package.json lists that no file imports and no script runs",
    heading: "Unused dependencies detected",
    threshold_unit: "",
    value_label: |_| "never used".to_string(),
    max_label: "",
    max_unit: "",
    ok_message: "No unused dependencies detected.",
};

/// Name of the unused catalog entries rule in `.oxicleanrc.json` and in
/// JSON and SARIF output, after knip's `catalog` issue type.
pub const CATALOG_RULE: &str = "catalog";

/// How unused catalog entry findings are worded in the terminal report.
pub const CATALOG_STYLE: ReportStyle = ReportStyle {
    description: "pnpm catalog entries no package.json refers to with catalog:",
    heading: "Unused catalog entries detected",
    threshold_unit: "",
    value_label: |_| "never referenced".to_string(),
    max_label: "",
    max_unit: "",
    ok_message: "No unused catalog entries detected.",
};

/// Name of the heavy dependencies rule in `.oxicleanrc.json` and in JSON
/// and SARIF output.
pub const HEAVY_RULE: &str = "heavy-dependencies";

/// Heavy dependencies threshold used when the config file sets none.
pub const HEAVY_DEFAULT_THRESHOLD: usize = 100;

/// How heavy dependency findings are worded in the terminal report.
pub const HEAVY_STYLE: ReportStyle = ReportStyle {
    description: "Packages whose import pulls in too many modules",
    heading: "Heavy dependencies detected",
    threshold_unit: "modules",
    value_label: |n| format!("pulls in {n} modules"),
    max_label: "Heaviest dependency",
    max_unit: "modules",
    ok_message: "No heavy dependencies detected.",
};

/// Name of the duplicate package versions rule in `.oxicleanrc.json` and in
/// JSON and SARIF output.
pub const VERSIONS_RULE: &str = "duplicate-versions";

/// Duplicate versions threshold used when the config file sets none: a
/// package at two versions is reported.
pub const VERSIONS_DEFAULT_THRESHOLD: usize = 2;

/// How duplicate version findings are worded in the terminal report.
pub const VERSIONS_STYLE: ReportStyle = ReportStyle {
    description: "Packages the lockfile installs at more than one version",
    heading: "Duplicate package versions detected",
    threshold_unit: "versions",
    value_label: |n| format!("{n} versions"),
    max_label: "Most versions",
    max_unit: "versions",
    ok_message: "No duplicate package versions detected.",
};
