use std::path::PathBuf;

use clap::Parser;
use oxiclean_core::{CommonArgs, report::ReportStyle};

/// Options for the `trace` command.
#[derive(Debug, Clone, Parser)]
#[command(name = "trace")]
#[command(about = "Report hot spots and duplicate packages in a tsc --generateTrace directory")]
pub struct Config {
    /// The directory `tsc --generateTrace <dir>` wrote. Defaults to
    /// `trace.dir` in `.oxicleanrc.json`, relative to the root
    #[arg(value_name = "DIR")]
    pub dir: Option<PathBuf>,

    #[command(flatten)]
    pub common: CommonArgs,
}

/// Name of the hot spots rule in `.oxicleanrc.json` and in JSON and SARIF
/// output.
pub const HOT_SPOTS_RULE: &str = "hot-spots";

/// Hot spots threshold, in milliseconds, used when the config file sets
/// none: analyze-trace's default `forceMillis`.
pub const HOT_SPOTS_DEFAULT_THRESHOLD: usize = 500;

/// Spans shorter than this many milliseconds are never hot spots, when the
/// config file sets no `skipMillis`: analyze-trace's default.
pub const DEFAULT_SKIP_MILLIS: usize = 100;

/// How hot spots are worded in the terminal report.
pub const HOT_SPOTS_STYLE: ReportStyle = ReportStyle {
    description: "Spans of a tsc trace where type checking or declaration emit took long",
    heading: "Hot spots in the trace",
    threshold_unit: "ms",
    value_label: |n| format!("{n}ms"),
    max_label: "Slowest",
    max_unit: "ms",
    ok_message: "No hot spots in the trace.",
};

/// Name of the duplicate packages rule in `.oxicleanrc.json` and in JSON
/// and SARIF output.
pub const DUPLICATE_PACKAGES_RULE: &str = "duplicate-packages";

/// Duplicate packages threshold, in directories, used when the config file
/// sets none.
pub const DUPLICATE_PACKAGES_DEFAULT_THRESHOLD: usize = 2;

/// How duplicate packages are worded in the terminal report.
pub const DUPLICATE_PACKAGES_STYLE: ReportStyle = ReportStyle {
    description: "Packages a tsc trace shows loaded from more than one directory",
    heading: "Packages tsc loads more than once",
    threshold_unit: "directories",
    value_label: |n| format!("{n} directories"),
    max_label: "Most copies",
    max_unit: "directories",
    ok_message: "No package loaded more than once.",
};
