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
//! - `tsconfig` ([`run_tsconfig_check`]): compiler options that slow
//!   every run, such as `skipLibCheck` left unset.
//! - `ambient-types` ([`run_ambient_types_check`]): programs that load
//!   `@types` packages nothing here declares.
//! - `isolated-declarations` ([`run_isolated_declarations_check`]):
//!   packages that could emit `.d.ts` files without type-checking, and JSR
//!   packages with slow types.
//! - `large-literals` ([`run_large_literals_check`]): object and array
//!   literals, and imported `.json` files, big enough that inferring
//!   their types slows `tsc`.
//! - `type-bloat` ([`run_type_bloat_check`]): package imports that load
//!   huge declaration trees.
//! - `complex-types` ([`run_complex_types_check`]): type shapes that make
//!   `tsc` build huge numbers of types.
//!
//! The `typecheck` command ([`run_typecheck_command`]) runs each of these
//! rules the config file leaves on, reading the tsconfig files once.

mod ambient_types;
mod command;
mod complex_types;
mod config;
mod declarations;
mod isolated_declarations;
mod large_literals;
mod lines;
mod overlap;
mod program_files;
pub mod program_map;
mod references;
mod scripts;
pub mod tsconfig;
mod tsconfig_options;
mod type_bloat;
mod typescript;

pub use ambient_types::run_ambient_types_check;
pub use command::{rules, run_typecheck_command};
pub use complex_types::{FlaggedType, flagged_types, flagged_types_in, run_complex_types_check};
pub use config::{
    AMBIENT_TYPES_RULE, AMBIENT_TYPES_STYLE, COMPLEX_TYPES_DEFAULT_THRESHOLD, COMPLEX_TYPES_RULE,
    COMPLEX_TYPES_STYLE, Config, DEFAULT_THRESHOLD, ISOLATED_DECLARATIONS_RULE,
    ISOLATED_DECLARATIONS_STYLE, LARGE_LITERALS_DEFAULT_THRESHOLD, LARGE_LITERALS_RULE,
    LARGE_LITERALS_STYLE, OVERLAP_RULE, OVERLAP_STYLE, PROGRAM_FILES_RULE, PROGRAM_FILES_STYLE,
    REFERENCES_RULE, REFERENCES_STYLE, TSCONFIG_RULE, TSCONFIG_STYLE, TYPE_BLOAT_DEFAULT_THRESHOLD,
    TYPE_BLOAT_RULE, TYPE_BLOAT_STYLE,
};
pub use isolated_declarations::run_isolated_declarations_check;
pub use large_literals::run_large_literals_check;
pub use overlap::run_overlap_check;
pub use oxiclean_core::{CheckResult, Finding, FindingKind, Issue};
pub use program_files::run_program_files_check;
pub use references::run_references_check;
pub use tsconfig::{TsConfigFile, TsConfigs};
pub use tsconfig_options::run_tsconfig_check;
pub use type_bloat::run_type_bloat_check;
