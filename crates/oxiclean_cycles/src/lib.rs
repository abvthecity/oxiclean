//! Circular import detection for JavaScript/TypeScript projects.
//!
//! Reports files that import each other, directly or indirectly, at runtime.
//! Cycles are found with a port of pnpm's graph sequencer
//! ([`oxiclean_core::sequence`]), which picks short cycles. After each round,
//! the import that closes each reported cycle is set aside and the search
//! repeats, so removing the last import of each reported cycle leaves no
//! cycles. Type-only imports never count, and dynamic `import()` counts only
//! when `includeDynamic` is set (which, unlike knip's `cycles.dynamicImports`,
//! applies to every check).
//!
//! # Examples
//!
//! ```no_run
//! use oxiclean_core::{CommonArgs, report};
//! use oxiclean_cycles::{Config, REPORT_STYLE, run_cycles_check};
//! use std::path::Path;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let cfg = Config { common: CommonArgs::with_root("/path/to/project") };
//! let result = run_cycles_check(&cfg)?;
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

pub use checker::{check, run_cycles_check};
pub use config::{Config, DEFAULT_THRESHOLD, REPORT_STYLE, RULE};
pub use oxiclean_core::{CheckResult, CycleFinding, CycleStep, Finding, FindingKind};
