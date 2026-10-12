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
    #[arg(long, default_value_t = 200)]
    pub threshold: usize,
}

/// How import bloat findings are worded in the terminal report.
pub const REPORT_STYLE: ReportStyle = ReportStyle {
    heading: "Import bloat detected",
    threshold_unit: "modules",
    value_label: |n| format!("{n} modules"),
    max_label: "Maximum bloat",
    max_unit: "modules",
    ok_message: "No bloat detected.",
};
