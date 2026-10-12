use clap::Parser;
use oxiclean_core::{CommonArgs, report::ReportStyle};

/// Options for the import cycle check.
#[derive(Debug, Clone, Parser)]
#[command(name = "import-cycles")]
#[command(about = "Check for circular imports in JavaScript/TypeScript projects")]
pub struct Config {
    #[command(flatten)]
    pub common: CommonArgs,
}

/// Name of this rule in `.oxicleanrc.json` and in JSON and SARIF output.
pub const RULE: &str = "import-cycles";

/// Threshold used when the config file sets none: a cycle of one file (a
/// file importing itself) or more, so every cycle is reported.
pub const DEFAULT_THRESHOLD: usize = 1;

/// How import cycle findings are worded in the terminal report.
pub const REPORT_STYLE: ReportStyle = ReportStyle {
    description: "Files that import each other, directly or indirectly, at runtime",
    heading: "Circular imports detected",
    threshold_unit: "files",
    value_label: |n| {
        if n == 1 { "imports itself".to_string() } else { format!("cycle of {n} files") }
    },
    max_label: "Longest cycle",
    max_unit: "files",
    ok_message: "No circular imports detected.",
};
