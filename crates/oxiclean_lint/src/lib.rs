//! Checks for ESLint and oxlint setups that make linting slower than it
//! needs to be.
//!
//! Lint configs are read without running them: JSON(C) files are parsed,
//! and JS/TS configs are read with oxc as far as they are plain data,
//! following constants, spreads, `defineConfig`, `tseslint.config` and
//! imports of the project's own files. The one rule,
//! `lint-config` ([`run_lint_config_check`]), reports typed linting
//! applied to JavaScript files, `parserOptions.project` set up the slow
//! way, rules that walk the whole import graph, ESLint running next to
//! oxlint without eslint-plugin-oxlint, build output the linter walks, and
//! oxlint's type-aware mode with no type-aware rule on.
//!
//! The `lint` command ([`run_lint_command`]) runs it unless the config
//! file turns it off.

mod command;
mod config;
mod configs;
mod js;
mod lint_config;
mod outputs;

pub use command::{rules, run_lint_command};
pub use config::{Config, DEFAULT_THRESHOLD, LINT_CONFIG_RULE, LINT_CONFIG_STYLE};
pub use lint_config::run_lint_config_check;
pub use oxiclean_core::{CheckResult, Finding, FindingKind, Issue};
