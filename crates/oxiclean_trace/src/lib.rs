//! Reads the output of `tsc --generateTrace <dir>` and reports what
//! `@typescript/analyze-trace` reports, joined to oxiclean's own findings.
//! oxiclean never runs `tsc`: it reads a trace directory that already
//! exists.
//!
//! - `hot-spots`: the hot-spot tree. Spans shorter than `skipMillis` are
//!   dropped; spans at least the threshold (analyze-trace's `forceMillis`)
//!   long are kept, and so are spans that take at least 60% of the span
//!   they're in. Files checked, expressions and declarations checked (at
//!   their lines and columns), types compared and variances measured (with
//!   each type's name, kind and declaration, from `types.json`), and
//!   declaration files that repeat `import("…")` types are reported, each
//!   with the frame it's in. A frame on what `complex-types` flags
//!   confirms that finding and is at least as severe.
//! - `duplicate-packages`: packages loaded from more than one
//!   `node_modules` directory, with each copy's version.
//!
//! Both a single program's `trace.json` and `types.json` and the
//! `legend.json`, `trace.N.json` and `types.N.json` of `tsc --build` are
//! read. Traces are streamed and only the records of `types.json` that a
//! hot spot names are kept, so multi-gigabyte traces are fine; a trace or
//! types file cut short keeps what came before the cut. Paths recorded on
//! another machine are found under the root, by the directory the trace
//! was recorded in.
//!
//! The `trace` command ([`run_trace_command`]) runs each rule the config
//! file leaves on; [`run_hot_spots_check`] and
//! [`run_duplicate_packages_check`] run one.
//!
//! # Examples
//!
//! ```no_run
//! use oxiclean_trace::{Config, run_hot_spots_check, run_trace_command};
//! use oxiclean_core::CommonArgs;
//!
//! # fn main() -> oxiclean_core::Result<()> {
//! // After `tsc -b --generateTrace trace` in /repo:
//! let cfg = Config { dir: Some("trace".into()), common: CommonArgs::with_root("/repo") };
//! // Each hot spot's file and milliseconds.
//! let slow: Vec<_> =
//!     run_hot_spots_check(&cfg)?.findings.into_iter().map(|f| (f.file, f.value)).collect();
//! # let _ = slow;
//! let all = run_trace_command(&cfg)?;
//! assert_eq!(all.rules.len(), 2);
//! # Ok(())
//! # }
//! ```

mod analyze;
mod command;
mod config;
mod duplicates;
mod hot_spots;
mod layout;
mod parse;
mod paths;
mod trace;
mod trivia;
mod types;

pub use command::{rules, run_duplicate_packages_check, run_hot_spots_check, run_trace_command};
pub use config::{
    Config, DEFAULT_SKIP_MILLIS, DUPLICATE_PACKAGES_DEFAULT_THRESHOLD, DUPLICATE_PACKAGES_RULE,
    DUPLICATE_PACKAGES_STYLE, HOT_SPOTS_DEFAULT_THRESHOLD, HOT_SPOTS_RULE, HOT_SPOTS_STYLE,
};
pub use oxiclean_core::{CheckResult, CommandResult, Finding, FindingKind};
