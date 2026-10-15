//! Import depth analysis for JavaScript/TypeScript projects.
//!
//! For every direct import of an entry file, this measures the longest chain
//! of imports below it. Deep chains slow module evaluation and resolution and
//! usually point at layering problems. Modules that import each other in a
//! cycle count as a single level, so results are stable across runs.
//!
//! # Examples
//!
//! ```no_run
//! use oxiclean_core::{CommonArgs, report};
//! use oxiclean_depth::{Config, REPORT_STYLE, run_depth_check};
//! use std::path::Path;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let cfg = Config { common: CommonArgs::with_root("/path/to/project") };
//! let result = run_depth_check(&cfg)?;
//!
//! let root = Path::new("/path/to/project");
//! let ctx = report::ReportContext { root, cwd: root };
//! let mut stdout = std::io::stdout().lock();
//! if result.findings.is_empty() {
//!     report::print_ok(&mut stdout, &REPORT_STYLE, result.threshold)?;
//! } else {
//!     report::print_findings(&mut stdout, &result.findings, &REPORT_STYLE, result.threshold, ctx)?;
//! }
//! # Ok(())
//! # }
//! ```

mod checker;
mod config;

pub use checker::{check, run_depth_check, run_depth_command};
pub use config::{Config, DEFAULT_THRESHOLD, REPORT_STYLE, RULE};
pub use oxiclean_core::{CheckResult, Finding, FindingKind};
