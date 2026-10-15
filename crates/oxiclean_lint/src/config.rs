use clap::Parser;
use oxiclean_core::{CommonArgs, report::ReportStyle};

/// Options for the lint setup checks.
#[derive(Debug, Clone, Parser)]
#[command(
    about = "Check for ESLint and oxlint setups that make linting slower than it needs to be"
)]
pub struct Config {
    #[command(flatten)]
    pub common: CommonArgs,
}

/// Every finding of these checks has a count as its value; the default
/// threshold of 1 reports each one.
pub const DEFAULT_THRESHOLD: usize = 1;

/// Name of the rule for lint configs that make linting slow.
pub const LINT_CONFIG_RULE: &str = "lint-config";

/// How lint config findings are worded in the terminal report.
pub const LINT_CONFIG_STYLE: ReportStyle = ReportStyle {
    description: "ESLint and oxlint configs that make linting slower than it needs to be: typed linting where it isn't needed, slow rules, and build output that isn't ignored",
    heading: "Lint configs that slow linting down",
    threshold_unit: "",
    value_label: |n| if n == 1 { "1 item".to_string() } else { format!("{n} items") },
    max_label: "",
    max_unit: "",
    ok_message: "No lint config slows linting down.",
};
