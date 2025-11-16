//! Import bloat detection for JavaScript/TypeScript projects.
//!
//! This crate analyzes import statements in JS/TS codebases to identify files
//! that import too many modules, which can lead to large bundle sizes.
//!
//! # Examples
//!
//! ## Basic Usage
//!
//! ```no_run
//! use oxiclean_import_bloat::{Config, run_import_bloat_check};
//! use std::io::{BufWriter, Write};
//!
//! # fn main() -> anyhow::Result<()> {
//! let cfg = Config {
//!     root: Some(std::path::PathBuf::from("/path/to/project")),
//!     threshold: 200,
//!     entry_glob: None,
//!     tsconfig_paths: Default::default(),
//! };
//!
//! let result = run_import_bloat_check(cfg.clone())?;
//!
//! if !result.warnings.is_empty() {
//!     // Use buffered output for better performance
//!     let mut stdout = BufWriter::new(std::io::stdout());
//!     oxiclean_import_bloat::print_warnings_tree(
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
mod collector;
mod config;
mod constants;
mod graph;
mod parser;
mod reporter;
mod resolver;
mod types;

// Re-export public API
pub use checker::run_import_bloat_check;
pub use config::Config;
pub use reporter::{print_no_bloat_message, print_warnings_tree};
pub use types::{CheckResult, Warning};
