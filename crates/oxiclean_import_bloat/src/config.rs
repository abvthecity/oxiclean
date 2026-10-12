use clap::Parser;
use oxiclean_core::{CommonArgs, report::ReportStyle};

/// Options for the import bloat check.
#[derive(Debug, Clone, Parser)]
#[command(name = "import-bloat")]
#[command(about = "Check for import bloat in JavaScript/TypeScript projects")]
pub struct Config {
    #[command(flatten)]
    pub common: CommonArgs,

    /// Flag imports and entries that reach at least this many unique modules
    /// [default: 200, or the config file's `threshold`]. Replaces every
    /// threshold in the config file, overrides included.
    #[arg(long)]
    pub threshold: Option<usize>,
}

/// Name of this rule in `oxiclean.toml` and in JSON and SARIF output.
pub const RULE: &str = "import-bloat";

/// Threshold used when neither the command line nor the config file sets one.
pub const DEFAULT_THRESHOLD: usize = 200;

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
