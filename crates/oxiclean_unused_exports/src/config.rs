use clap::Parser;
use oxiclean_core::{CommonArgs, report::ReportStyle};

/// Options for the unused exports and unused types checks.
#[derive(Debug, Clone, Parser)]
#[command(
    about = "Check for exports, and members of exported enums and namespaces, that nothing uses"
)]
pub struct Config {
    #[command(flatten)]
    pub common: CommonArgs,
}

/// Name of the unused exports rule in `.oxicleanrc.json` and in JSON and
/// SARIF output.
pub const UNUSED_EXPORTS_RULE: &str = "unused-exports";

/// Name of the unused types rule.
pub const UNUSED_TYPES_RULE: &str = "unused-types";

/// Name of the unused enum members rule.
pub const UNUSED_ENUM_MEMBERS_RULE: &str = "unused-enum-members";

/// Name of the unused namespace members rule.
pub const UNUSED_NAMESPACE_MEMBERS_RULE: &str = "unused-namespace-members";

/// Threshold used when the config file sets none. Every finding has the
/// value 1, so every unused export is reported.
pub const DEFAULT_THRESHOLD: usize = 1;

/// How unused export findings are worded in the terminal report.
pub const UNUSED_EXPORTS_STYLE: ReportStyle = ReportStyle {
    description: "Exports that no other file imports",
    heading: "Unused exports detected",
    threshold_unit: "",
    value_label: |_| "never imported".to_string(),
    max_label: "",
    max_unit: "",
    ok_message: "No unused exports detected.",
};

/// How unused type findings are worded in the terminal report.
pub const UNUSED_TYPES_STYLE: ReportStyle = ReportStyle {
    description: "Exported types, interfaces and enums that no other file imports",
    heading: "Unused exported types detected",
    threshold_unit: "",
    value_label: |_| "never imported".to_string(),
    max_label: "",
    max_unit: "",
    ok_message: "No unused exported types detected.",
};

/// How unused enum member findings are worded in the terminal report.
pub const UNUSED_ENUM_MEMBERS_STYLE: ReportStyle = ReportStyle {
    description: "Members of used enums that nothing uses",
    heading: "Unused enum members detected",
    threshold_unit: "",
    value_label: |_| "never used".to_string(),
    max_label: "",
    max_unit: "",
    ok_message: "No unused enum members detected.",
};

/// How unused namespace member findings are worded in the terminal report.
pub const UNUSED_NAMESPACE_MEMBERS_STYLE: ReportStyle = ReportStyle {
    description: "What used namespaces export that nothing uses",
    heading: "Unused namespace members detected",
    threshold_unit: "",
    value_label: |_| "never used".to_string(),
    max_label: "",
    max_unit: "",
    ok_message: "No unused namespace members detected.",
};
