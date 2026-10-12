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
pub const UNUSED_DEPENDENCIES_RULE: &str = "unused-dependencies";

/// Name of the unused devDependencies rule.
pub const UNUSED_DEV_DEPENDENCIES_RULE: &str = "unused-dev-dependencies";

/// Threshold used when the config file sets none. Every finding has the
/// value 1, so every unused package is reported.
pub const DEFAULT_THRESHOLD: usize = 1;

/// How unused dependency findings are worded in the terminal report.
pub const UNUSED_DEPENDENCIES_STYLE: ReportStyle = ReportStyle {
    description: "Packages in dependencies that no file imports and no script runs",
    heading: "Unused dependencies detected",
    threshold_unit: "",
    value_label: |_| "never used".to_string(),
    max_label: "",
    max_unit: "",
    ok_message: "No unused dependencies detected.",
};

/// How unused devDependency findings are worded in the terminal report.
pub const UNUSED_DEV_DEPENDENCIES_STYLE: ReportStyle = ReportStyle {
    description: "Packages in devDependencies that no file imports and no script runs",
    heading: "Unused devDependencies detected",
    threshold_unit: "",
    value_label: |_| "never used".to_string(),
    max_label: "",
    max_unit: "",
    ok_message: "No unused devDependencies detected.",
};
