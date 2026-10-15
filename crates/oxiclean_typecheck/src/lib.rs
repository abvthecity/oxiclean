//! Checks for setups that make `tsc` and typed linting slower than they
//! need to be.
//!
//! `tsc`'s cost grows with the files in each program, times the number of
//! programs that load them, times the work per file. These checks read
//! every tsconfig the way `tsc` does ([`TsConfigs`]) and report:
//!
//! - `program-files` ([`run_program_files_check`]): files a program
//!   checks that nobody needs checked, such as another build's output.
//! - `references` ([`run_references_check`]): project references that
//!   don't match what packages import, and package source that several
//!   packages type-check.
//! - `overlap` ([`run_overlap_check`]): files more than one program
//!   checks, so they are checked more than once.

mod config;
mod overlap;
mod program_files;
mod references;
pub mod tsconfig;

pub use config::{
    Config, DEFAULT_THRESHOLD, OVERLAP_RULE, OVERLAP_STYLE, PROGRAM_FILES_RULE,
    PROGRAM_FILES_STYLE, REFERENCES_RULE, REFERENCES_STYLE,
};
pub use overlap::run_overlap_check;
pub use oxiclean_core::{CheckResult, Finding, FindingKind, Issue};
pub use program_files::run_program_files_check;
pub use references::run_references_check;
pub use tsconfig::{TsConfigFile, TsConfigs};
