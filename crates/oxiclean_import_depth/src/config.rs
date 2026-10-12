use clap::Parser;
use oxiclean_core::{CommonArgs, report::ReportStyle};

/// Options for the import depth check.
#[derive(Debug, Clone, Parser)]
#[command(name = "import-depth")]
#[command(about = "Check for excessive import depth in JavaScript/TypeScript projects")]
pub struct Config {
    #[command(flatten)]
    pub common: CommonArgs,

    /// Flag imports whose import chain is at least this deep
    /// [default: 10, or the config file's `threshold`]. Replaces every
    /// threshold in the config file, overrides included.
    #[arg(long)]
    pub threshold: Option<usize>,
}

/// Name of this rule in `oxiclean.toml` and in JSON and SARIF output.
pub const RULE: &str = "import-depth";

/// Threshold used when neither the command line nor the config file sets one.
pub const DEFAULT_THRESHOLD: usize = 10;

/// How import depth findings are worded in the terminal report.
pub const REPORT_STYLE: ReportStyle = ReportStyle {
    description: "Imports with too deep a chain of imports below them",
    heading: "Excessive import depth detected",
    threshold_unit: "levels",
    value_label: |n| format!("depth: {n}"),
    max_label: "Maximum depth",
    max_unit: "levels",
    ok_message: "No excessive import depth detected.",
};
