use clap::Parser;
use oxiclean_core::{CommonArgs, report::ReportStyle};

/// Options for the tsconfig and lint setup checks.
#[derive(Debug, Clone, Parser)]
#[command(about = "Check for tsconfig and lint setups that slow down type checking")]
pub struct Config {
    #[command(flatten)]
    pub common: CommonArgs,
}

/// Every finding of these checks has a count as its value; the default
/// threshold of 1 reports each one.
pub const DEFAULT_THRESHOLD: usize = 1;

/// Name of the rule for files a tsconfig program checks for nothing.
pub const PROGRAM_FILES_RULE: &str = "program-files";

/// How program file findings are worded in the terminal report.
pub const PROGRAM_FILES_STYLE: ReportStyle = ReportStyle {
    description: "Files a tsconfig program type-checks that no one needs checked, such as build output",
    heading: "tsconfig programs check files they don't need",
    threshold_unit: "",
    value_label: files,
    max_label: "Most needless files",
    max_unit: "files",
    ok_message: "No tsconfig program checks files it doesn't need.",
};

/// Name of the rule for files several tsconfig programs check.
pub const OVERLAP_RULE: &str = "overlap";

/// How overlap findings are worded in the terminal report.
pub const OVERLAP_STYLE: ReportStyle = ReportStyle {
    description: "Files that more than one tsconfig program type-checks",
    heading: "Files type-checked by more than one tsconfig",
    threshold_unit: "",
    value_label: files,
    max_label: "Most shared files",
    max_unit: "files",
    ok_message: "No file is type-checked by more than one tsconfig.",
};

/// Name of the rule for project references that don't match imports.
pub const REFERENCES_RULE: &str = "references";

/// How reference findings are worded in the terminal report.
pub const REFERENCES_STYLE: ReportStyle = ReportStyle {
    description: "Project references that don't match what packages import, so packages are type-checked more than once",
    heading: "Project references don't match imports",
    threshold_unit: "",
    value_label: |n| if n == 1 { "1 config".to_string() } else { format!("{n} configs") },
    max_label: "",
    max_unit: "",
    ok_message: "Project references match imports.",
};

/// `1 file` or `n files`.
fn files(n: usize) -> String {
    if n == 1 { "1 file".to_string() } else { format!("{n} files") }
}

/// Name of the rule for compiler options that slow `tsc` down.
pub const TSCONFIG_RULE: &str = "tsconfig";

/// How tsconfig option findings are worded in the terminal report.
pub const TSCONFIG_STYLE: ReportStyle = ReportStyle {
    description: "Compiler options that make every tsc run slower than it needs to be",
    heading: "tsconfig options slow tsc down",
    threshold_unit: "",
    value_label: |n| if n == 1 { "1 config".to_string() } else { format!("{n} configs") },
    max_label: "",
    max_unit: "",
    ok_message: "No tsconfig option slows tsc down.",
};

/// Name of the rule for programs that load every visible `@types` package.
pub const AMBIENT_TYPES_RULE: &str = "ambient-types";

/// How ambient types findings are worded in the terminal report.
pub const AMBIENT_TYPES_STYLE: ReportStyle = ReportStyle {
    description: "tsconfig programs without compilerOptions.types, which load every @types package they can see, including ones nothing here declares",
    heading: "Programs load @types packages they don't declare",
    threshold_unit: "",
    value_label: |n| if n == 1 { "1 package".to_string() } else { format!("{n} packages") },
    max_label: "Most undeclared @types",
    max_unit: "packages",
    ok_message: "Every program loads only the @types packages it declares.",
};
