use clap::Parser;
use oxiclean_core::{CommonArgs, report::ReportStyle};

/// Options for the import depth check.
#[derive(Debug, Clone, Parser)]
#[command(name = "depth")]
#[command(about = "Check for excessive import depth in JavaScript/TypeScript projects")]
pub struct Config {
    #[command(flatten)]
    pub common: CommonArgs,
}

/// Name of this rule in `.oxicleanrc.json` and in JSON and SARIF output.
pub const RULE: &str = "depth";

/// Threshold used when the config file sets none.
pub const DEFAULT_THRESHOLD: usize = 20;

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
