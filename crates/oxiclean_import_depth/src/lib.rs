//! Import depth analysis for JavaScript/TypeScript projects.
//!
//! This crate analyzes import statements in JS/TS codebases to identify files
//! that have excessive import depth (number of module traversals), which can
//! indicate complex dependency chains and slow module resolution.
//!
//! # Examples
//!
//! ## Basic Usage
//!
//! ```no_run
//! use oxiclean_import_depth::{Config, run_import_depth_check};
//! use std::io::{BufWriter, Write};
//!
//! # fn main() -> anyhow::Result<()> {
//! let cfg = Config {
//!     root: Some(std::path::PathBuf::from("/path/to/project")),
//!     threshold: 10,
//!     entry_glob: None,
//!     tsconfig_paths: Default::default(),
//! };
//!
//! let result = run_import_depth_check(cfg.clone())?;
//!
//! if !result.warnings.is_empty() {
//!     // Use buffered output for better performance
//!     let mut stdout = BufWriter::new(std::io::stdout());
//!     oxiclean_import_depth::print_warnings_tree(
//!         &mut stdout,
//!         &result.warnings,
//!         &cfg,
//!         cfg.threshold,
//!     )?;
//!     stdout.flush()?;
//! }
//! # Ok(())
//! # }
//! ```

mod checker;
mod config;
mod depth;
mod reporter;
mod types;

// Re-export public API
pub use checker::run_import_depth_check;
pub use config::Config;
pub use reporter::{print_no_depth_issues_message, print_warnings_tree};
pub use types::{CheckResult, Warning};
