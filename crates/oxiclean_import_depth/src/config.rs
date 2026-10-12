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
    #[arg(long, default_value_t = 10)]
    pub threshold: usize,
}

/// How import depth findings are worded in the terminal report.
pub const REPORT_STYLE: ReportStyle = ReportStyle {
    heading: "Excessive import depth detected",
    threshold_unit: "levels",
    value_label: |n| format!("depth: {n}"),
    max_label: "Maximum depth",
    max_unit: "levels",
    ok_message: "No excessive import depth detected.",
};
