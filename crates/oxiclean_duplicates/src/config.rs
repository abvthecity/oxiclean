use clap::Parser;
use oxiclean_core::{CommonArgs, report::ReportStyle};

/// Options for the duplicate exports check.
#[derive(Debug, Clone, Parser)]
#[command(name = "duplicates")]
#[command(about = "Check for modules that export the same thing under several names")]
pub struct Config {
    #[command(flatten)]
    pub common: CommonArgs,
}

/// Name of this rule in `.oxicleanrc.json` and in JSON and SARIF output.
pub const RULE: &str = "duplicates";

/// Threshold used when the config file sets none: two names for one thing.
pub const DEFAULT_THRESHOLD: usize = 2;

/// How duplicate export findings are worded in the terminal report.
pub const REPORT_STYLE: ReportStyle = ReportStyle {
    description: "Modules that export the same thing under more than one name",
    heading: "Duplicate exports detected",
    threshold_unit: "names",
    value_label: |n| format!("exported as {n} names"),
    max_label: "Most names for one export",
    max_unit: "names",
    ok_message: "No duplicate exports detected.",
};
