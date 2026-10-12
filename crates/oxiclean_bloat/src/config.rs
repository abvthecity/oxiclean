use clap::Parser;
use oxiclean_core::{CommonArgs, report::ReportStyle};

/// Options for the import bloat check.
#[derive(Debug, Clone, Parser)]
#[command(name = "bloat")]
#[command(about = "Check for import bloat in JavaScript/TypeScript projects")]
pub struct Config {
    #[command(flatten)]
    pub common: CommonArgs,
}

/// Name of this rule in `.oxicleanrc.json` and in JSON and SARIF output.
pub const RULE: &str = "bloat";

/// Threshold used when the config file sets none.
pub const DEFAULT_THRESHOLD: usize = 300;

/// How import bloat findings are worded in the terminal report.
pub const REPORT_STYLE: ReportStyle = ReportStyle {
    description: "Imports that pull in too many modules",
    heading: "Import bloat detected",
    threshold_unit: "modules",
    value_label: |n| format!("{n} modules"),
    max_label: "Maximum bloat",
    max_unit: "modules",
    ok_message: "No bloat detected.",
};
