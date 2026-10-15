//! Unused export detection for JavaScript/TypeScript projects.
//!
//! Reports exports that no other file imports, like knip's `exports` and
//! `types` issue types: `unused-exports` covers values, functions, classes
//! and namespaces, and `unused-types` covers types, interfaces and enums.
//! `unused-enum-members` and `unused-namespace-members` report the members
//! of used enums and namespaces that nothing uses, like knip's
//! `enumMembers` and `namespaceMembers`.
//! Exports of entry files are the project's interface and are never
//! reported, and neither are exports tagged `@public` (or `@beta`, or
//! `@alias`) in their JSDoc.
//!
//! The `unused` command ([`run_unused_command`]) runs each of the four
//! rules the config file leaves on, from one module graph.
//!
//! # Examples
//!
//! ```no_run
//! use oxiclean_core::{CommonArgs, report};
//! use oxiclean_unused_exports::{Config, UNUSED_EXPORTS_STYLE, run_unused_exports_check};
//! use std::path::Path;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let cfg = Config { common: CommonArgs::with_root("/path/to/project") };
//! let result = run_unused_exports_check(&cfg)?;
//!
//! let root = Path::new("/path/to/project");
//! let ctx = report::ReportContext { root, cwd: root };
//! let mut stdout = std::io::stdout().lock();
//! if result.findings.is_empty() {
//!     report::print_ok(&mut stdout, &UNUSED_EXPORTS_STYLE, result.threshold)?;
//! } else {
//!     let style = &UNUSED_EXPORTS_STYLE;
//!     report::print_findings(&mut stdout, &result.findings, style, result.threshold, ctx)?;
//! }
//! # Ok(())
//! # }
//! ```

mod checker;
mod config;

pub use checker::{
    ExportUsage, Unused, check, check_usage, run_unused_command, run_unused_enum_members_check,
    run_unused_exports_check, run_unused_namespace_members_check, run_unused_types_check,
    unused_exports, unused_members,
};
pub use config::{
    Config, DEFAULT_THRESHOLD, UNUSED_ENUM_MEMBERS_RULE, UNUSED_ENUM_MEMBERS_STYLE,
    UNUSED_EXPORTS_RULE, UNUSED_EXPORTS_STYLE, UNUSED_NAMESPACE_MEMBERS_RULE,
    UNUSED_NAMESPACE_MEMBERS_STYLE, UNUSED_TYPES_RULE, UNUSED_TYPES_STYLE,
};
pub use oxiclean_core::{CheckResult, Finding, FindingKind, UnusedExport};
