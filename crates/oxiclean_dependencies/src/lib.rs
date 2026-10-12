//! Unused dependency detection for JavaScript/TypeScript projects.
//!
//! Reports the packages the root's and each workspace package's
//! package.json lists that no file imports and no script runs, like knip's
//! `dependencies` and `devDependencies` issue types with its plugins off,
//! as one rule, `dependencies`, whose `fields` option picks
//! `dependencies`, `devDependencies` or both (the default). See
//! [`unused_dependencies`] for what counts as a use.
//!
//! # Examples
//!
//! ```no_run
//! use oxiclean_core::{CommonArgs, report};
//! use oxiclean_dependencies::{
//!     Config, DEPENDENCIES_STYLE, run_dependencies_check,
//! };
//! use std::path::Path;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let cfg = Config { common: CommonArgs::with_root("/path/to/project") };
//! let result = run_dependencies_check(&cfg)?;
//!
//! let root = Path::new("/path/to/project");
//! let ctx = report::ReportContext { root, cwd: root };
//! let mut stdout = std::io::stdout().lock();
//! let style = &DEPENDENCIES_STYLE;
//! if result.findings.is_empty() {
//!     report::print_ok(&mut stdout, style, result.threshold)?;
//! } else {
//!     report::print_findings(&mut stdout, &result.findings, style, result.threshold, ctx)?;
//! }
//! # Ok(())
//! # }
//! ```

mod checker;
mod config;
mod scripts;

pub use checker::{Unused, check, run_dependencies_check, unused_dependencies};
pub use config::{Config, DEFAULT_THRESHOLD, DEPENDENCIES_RULE, DEPENDENCIES_STYLE};
pub use oxiclean_core::{CheckResult, DependencyField, Finding, FindingKind, UnusedDependency};
