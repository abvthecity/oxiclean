//! Duplicate export detection for JavaScript/TypeScript projects.
//!
//! Reports modules that export the same thing under more than one name, like
//! knip's `duplicates` issue type: `export const b = a` or `export default a`
//! where `a` is already an export of the module. Importers then reach one
//! value through two names, which makes renames and unused-export checks
//! miss half the picture. Tag an intentional alias with `@alias` in its JSDoc
//! to keep it out of the report.
//!
//! # Examples
//!
//! ```no_run
//! use oxiclean_core::{CommonArgs, report};
//! use oxiclean_duplicates::{Config, REPORT_STYLE, run_duplicates_check};
//! use std::path::Path;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let cfg = Config { common: CommonArgs::with_root("/path/to/project") };
//! let result = run_duplicates_check(&cfg)?;
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

pub use checker::{check, duplicates, run_duplicates_check, run_duplicates_command};
pub use config::{Config, DEFAULT_THRESHOLD, REPORT_STYLE, RULE};
pub use oxiclean_core::{CheckResult, DuplicateExport, ExportName, Finding, FindingKind};
