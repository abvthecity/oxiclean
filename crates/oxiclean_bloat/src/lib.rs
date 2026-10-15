//! Import bloat detection for JavaScript/TypeScript projects.
//!
//! For every entry file, this counts how many unique modules each of its
//! imports pulls in (transitively), and flags imports and entries whose count
//! reaches a threshold. Large counts usually mean a barrel file or a heavy
//! dependency is dragged into a bundle. The `barrels` rule
//! ([`run_barrels_check`]) reports barrel files whose import pulls in many
//! modules, and the `bloat` command ([`run_bloat_command`]) runs each of the
//! two rules the config file leaves on, from one module graph.
//!
//! # Examples
//!
//! ```no_run
//! use oxiclean_core::{CommonArgs, report};
//! use oxiclean_bloat::{Config, REPORT_STYLE, run_bloat_check};
//! use std::path::Path;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let cfg = Config { common: CommonArgs::with_root("/path/to/project") };
//! let result = run_bloat_check(&cfg)?;
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

mod barrels;
mod checker;
mod command;
mod config;

pub use barrels::{check_barrels, run_barrels_check};
pub use checker::{check, run_bloat_check};
pub use command::run_bloat_command;
pub use config::{
    BARRELS_DEFAULT_THRESHOLD, BARRELS_RULE, BARRELS_STYLE, Config, DEFAULT_THRESHOLD,
    REPORT_STYLE, RULE,
};
pub use oxiclean_core::{CheckResult, Finding, FindingKind};
