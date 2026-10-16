use std::{
    io::{BufWriter, Write},
    process::ExitCode,
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use colored::Colorize;
use oxiclean_core::{
    CommandResult, RuleResult, fix,
    report::{self, OutputFormat, ReportContext, ReportMeta},
};
use tracing::debug;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "oxiclean", version)]
#[command(about = "A collection of tools for cleaning up codebases", long_about = None)]
#[command(after_help = "Each command runs a group of rules. `.oxicleanrc.json` turns each rule \
                        on or off and sets its severity and threshold.")]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Output format
    #[arg(long, value_enum, default_value_t = OutputFormat::Text, global = true)]
    format: OutputFormat,

    /// Apply the fixes findings offer, then report what is left
    #[arg(long, global = true)]
    fix: bool,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Check for exports, types and enum and namespace members nothing uses
    ///
    /// Runs the rules unused-exports, unused-types, unused-enum-members and
    /// unused-namespace-members, each unless `.oxicleanrc.json` turns it off.
    #[command(name = "unused")]
    Unused(oxiclean_unused_exports::Config),
    /// Check for unused, heavy and duplicated packages
    ///
    /// Runs the rules dependencies (unused packages, error by default),
    /// heavy-dependencies (warn by default), catalog and duplicate-versions,
    /// each unless `.oxicleanrc.json` turns it off.
    #[command(name = "dependencies")]
    Dependencies(oxiclean_dependencies::Config),
    /// Check for tsconfig setups that make type checking slower than it needs to be
    ///
    /// Runs the rules program-files, overlap, references, tsconfig,
    /// ambient-types, isolated-declarations, large-literals, type-bloat and
    /// complex-types, each unless `.oxicleanrc.json` turns it off.
    #[command(name = "typecheck")]
    Typecheck(oxiclean_typecheck::Config),
    /// Check for ESLint and oxlint configs that make linting slower than it needs to be
    ///
    /// Runs the rule lint-config (warn by default) unless `.oxicleanrc.json`
    /// turns it off. Configs are read without running ESLint or oxlint.
    #[command(name = "lint")]
    Lint(oxiclean_lint::Config),
    /// Check for imports and barrel files that pull in too many modules
    ///
    /// Runs the rules bloat and barrels, each unless `.oxicleanrc.json` turns
    /// it off.
    #[command(name = "bloat")]
    Bloat(oxiclean_bloat::Config),
    /// Report hot spots and duplicate packages in a `tsc --generateTrace` directory
    ///
    /// Reads the trace `tsc --generateTrace <dir>` wrote (oxiclean never
    /// runs tsc) and runs the rules hot-spots (warn by default; an error
    /// where the trace confirms a complex-types finding) and
    /// duplicate-packages (warn by default), each unless `.oxicleanrc.json`
    /// turns it off.
    #[command(name = "trace")]
    Trace(oxiclean_trace::Config),
    /// Check for excessive import depth in JavaScript/TypeScript projects
    #[command(name = "depth")]
    Depth(oxiclean_depth::Config),
    /// Check for circular imports in JavaScript/TypeScript projects
    #[command(name = "cycles")]
    Cycles(oxiclean_cycles::Config),
    /// Check for modules that export the same thing under several names
    #[command(name = "duplicates")]
    Duplicates(oxiclean_duplicates::Config),
    /// Explore which tsconfig programs type-check each package, and why
    ///
    /// Opens an interactive view: how much type checking repeats work
    /// another program already did, the causes of the repeats, the packages
    /// they cost most, and for each package the programs that check it and
    /// how they reach it. With --format json, prints the map instead.
    #[command(name = "map")]
    Map(MapConfig),
    /// Score the project's health and rank findings by the time fixing them saves
    ///
    /// Runs every command's rules (trace only when `trace.dir` is set in
    /// `.oxicleanrc.json`) and prints a health score from 0 to 100, what the
    /// findings cost developers and CI a week, and the findings that cost
    /// the most. `savings` in `.oxicleanrc.json` describes the team.
    #[command(name = "health")]
    Health(HealthConfig),
}

/// Options for `oxiclean map`.
#[derive(Debug, Clone, clap::Args)]
struct MapConfig {
    #[command(flatten)]
    common: oxiclean_core::CommonArgs,
}

/// Opens the program map, or prints it in `format`.
fn run_map(cfg: &MapConfig, format: OutputFormat) -> Result<()> {
    match format {
        OutputFormat::Text => {
            // Diagnostics on stderr would draw over the view.
            QUIET.store(true, Ordering::Relaxed);
            let shown = oxiclean_tui::run_map(cfg.common.clone());
            QUIET.store(false, Ordering::Relaxed);
            shown
        }
        OutputFormat::Json => {
            let progress = oxiclean_typecheck::program_map::Progress::default();
            let map = oxiclean_typecheck::program_map::scan(&cfg.common, &progress)?;
            let mut stdout = BufWriter::new(std::io::stdout().lock());
            serde_json::to_writer_pretty(&mut stdout, &map)?;
            writeln!(stdout)?;
            stdout.flush()?;
            Ok(())
        }
        OutputFormat::Sarif => {
            anyhow::bail!("`map` has no findings to write as SARIF; use --format json")
        }
    }
}

/// Options for `oxiclean health`.
#[derive(Debug, Clone, clap::Args)]
struct HealthConfig {
    #[command(flatten)]
    common: oxiclean_core::CommonArgs,
}

/// A rule named `rule` that couldn't run because of `e`.
fn failed(
    result: &CommandResult,
    rule: &'static str,
    style: oxiclean_core::report::ReportStyle,
    e: &oxiclean_core::Error,
) -> RuleResult {
    RuleResult::failed(rule, style, result.root.clone(), oxiclean_core::command::describe(e))
}

impl HealthConfig {
    /// Runs every command, each on the same root and config file, and joins
    /// what their rules found.
    fn run(&self) -> oxiclean_core::Result<CommandResult> {
        let workspace = self.common.workspace()?;
        let mut result = CommandResult::of_workspace(&workspace);
        let common = || self.common.clone();
        let groups = [
            (
                "unused",
                oxiclean_unused_exports::UNUSED_EXPORTS_STYLE,
                oxiclean_unused_exports::run_unused_command(&oxiclean_unused_exports::Config {
                    common: common(),
                }),
            ),
            (
                "dependencies",
                oxiclean_dependencies::DEPENDENCIES_STYLE,
                oxiclean_dependencies::run_dependencies_command(&oxiclean_dependencies::Config {
                    common: common(),
                }),
            ),
            (
                "typecheck",
                oxiclean_typecheck::TSCONFIG_STYLE,
                oxiclean_typecheck::run_typecheck_command(&oxiclean_typecheck::Config {
                    common: common(),
                }),
            ),
            (
                "lint",
                oxiclean_lint::LINT_CONFIG_STYLE,
                oxiclean_lint::run_lint_command(&oxiclean_lint::Config { common: common() }),
            ),
            (
                "bloat",
                oxiclean_bloat::REPORT_STYLE,
                oxiclean_bloat::run_bloat_command(&oxiclean_bloat::Config { common: common() }),
            ),
            (
                "depth",
                oxiclean_depth::REPORT_STYLE,
                oxiclean_depth::run_depth_command(&oxiclean_depth::Config { common: common() }),
            ),
            (
                "cycles",
                oxiclean_cycles::REPORT_STYLE,
                oxiclean_cycles::run_cycles_command(&oxiclean_cycles::Config { common: common() }),
            ),
            (
                "duplicates",
                oxiclean_duplicates::REPORT_STYLE,
                oxiclean_duplicates::run_duplicates_command(&oxiclean_duplicates::Config {
                    common: common(),
                }),
            ),
        ];
        for (command, style, run) in groups {
            match run {
                Ok(found) => result.extend(found),
                // A command whose rules all need a module graph can't run
                // without entry files; the other commands still can.
                Err(e @ oxiclean_core::Error::NoEntries(_)) => {
                    result.rules.push(failed(&result, command, style, &e));
                }
                Err(e) => return Err(e),
            }
        }
        // A trace is something a person records, so only a configured one
        // is read, and one that isn't there yet fails only its rules.
        if workspace.config.trace.dir.is_some() {
            let trace = oxiclean_trace::Config { dir: None, common: common() };
            match oxiclean_trace::run_trace_command(&trace) {
                Ok(found) => result.extend(found),
                Err(
                    e @ (oxiclean_core::Error::Trace { .. } | oxiclean_core::Error::NoTraceDir),
                ) => {
                    let style = oxiclean_trace::HOT_SPOTS_STYLE;
                    result.rules.push(failed(&result, "trace", style, &e));
                }
                Err(e) => return Err(e),
            }
        }
        Ok(result)
    }
}

impl Commands {
    /// The command's name, as typed.
    fn name(&self) -> &'static str {
        match self {
            Self::Unused(_) => "unused",
            Self::Dependencies(_) => "dependencies",
            Self::Typecheck(_) => "typecheck",
            Self::Lint(_) => "lint",
            Self::Bloat(_) => "bloat",
            Self::Trace(_) => "trace",
            Self::Depth(_) => "depth",
            Self::Cycles(_) => "cycles",
            Self::Duplicates(_) => "duplicates",
            Self::Health(_) => "health",
            Self::Map(_) => "map",
        }
    }

    /// Runs each of the command's rules the config file leaves on.
    fn run(&self) -> oxiclean_core::Result<CommandResult> {
        match self {
            Self::Unused(cfg) => oxiclean_unused_exports::run_unused_command(cfg),
            Self::Dependencies(cfg) => oxiclean_dependencies::run_dependencies_command(cfg),
            Self::Typecheck(cfg) => oxiclean_typecheck::run_typecheck_command(cfg),
            Self::Lint(cfg) => oxiclean_lint::run_lint_command(cfg),
            Self::Bloat(cfg) => oxiclean_bloat::run_bloat_command(cfg),
            Self::Trace(cfg) => oxiclean_trace::run_trace_command(cfg),
            Self::Depth(cfg) => oxiclean_depth::run_depth_command(cfg),
            Self::Cycles(cfg) => oxiclean_cycles::run_cycles_command(cfg),
            Self::Duplicates(cfg) => oxiclean_duplicates::run_duplicates_command(cfg),
            Self::Health(cfg) => cfg.run(),
            // A view, with no findings; `run` opens it instead.
            Self::Map(cfg) => Ok(CommandResult::of_workspace(&cfg.common.workspace()?)),
        }
    }
}

/// The stack of rayon's threads and of the thread the command runs on.
/// oxc's parser recurses once per level of nesting, and the defaults
/// (2 MiB for rayon, 8 MiB for the main thread) overflow on deeply nested
/// code, such as a type with thousands of nested conditionals.
const THREAD_STACK: usize = 64 * 1024 * 1024;

fn main() -> Result<ExitCode> {
    init_tracing();
    // Parsing also happens off rayon's pool, so the whole command runs on a
    // thread with the same large stack.
    std::thread::Builder::new()
        .stack_size(THREAD_STACK)
        .spawn(run)
        .context("could not start the main thread")?
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

fn run() -> Result<ExitCode> {
    if let Err(e) = rayon::ThreadPoolBuilder::new().stack_size(THREAD_STACK).build_global() {
        debug!("Using rayon's default thread pool: {e}");
    }
    let cli = Cli::parse();
    debug!("Parsed CLI arguments: {:?}", cli.command);

    // A score shouldn't rewrite the project; each command fixes its own.
    if cli.fix && matches!(cli.command, Commands::Health(_)) {
        anyhow::bail!("`health` only reports; run --fix with the command whose findings to fix");
    }
    if let Commands::Map(cfg) = &cli.command {
        if cli.fix {
            anyhow::bail!("`map` only shows; run --fix with the command whose findings to fix");
        }
        run_map(cfg, cli.format)?;
        return Ok(ExitCode::SUCCESS);
    }
    let start = Instant::now();
    let command = cli.command.name();
    let mut result = cli.command.run()?;
    // A fix can make another possible, so the command runs again after
    // each round of fixes until no new fix applies.
    let fixed = if cli.fix {
        fix::apply_command_fixes_until_settled(&mut result, || cli.command.run())
            .context("could not apply fixes")?
    } else {
        0
    };
    let elapsed_ms = start.elapsed().as_millis();

    let cwd = std::env::current_dir().context("could not read the current directory")?;
    let ctx = ReportContext { root: &result.root, cwd: &cwd };

    // stdout is line-buffered; a BufWriter avoids a syscall per line.
    // See https://github.com/rust-lang/rust/issues/60673
    let mut stdout = BufWriter::new(std::io::stdout().lock());
    let meta = ReportMeta { command, version: env!("CARGO_PKG_VERSION"), elapsed_ms };
    match cli.format {
        OutputFormat::Text => {
            let threads = rayon::current_num_threads();
            if matches!(cli.command, Commands::Health(_)) {
                report::print_health(&mut stdout, &result, elapsed_ms, threads, ctx)?;
            } else {
                report::print_command(&mut stdout, command, &result, elapsed_ms, threads, ctx)?;
            }
            if cli.fix {
                writeln!(
                    stdout,
                    "{} Fixed {} findings.",
                    "●".bright_blue(),
                    fixed.to_string().cyan()
                )?;
            }
        }
        OutputFormat::Json => report::write_json(&mut stdout, meta, &result)?,
        OutputFormat::Sarif => report::write_sarif(&mut stdout, meta, &result)?,
    }
    stdout.flush()?;

    // A non-zero exit fails CI when a finding with error severity crossed its
    // threshold, in any of the command's rules, or a rule couldn't run;
    // warnings alone don't.
    Ok(if result.fails() { ExitCode::FAILURE } else { ExitCode::SUCCESS })
}

/// Sends diagnostics to stderr so stdout carries only the report. `RUST_LOG`
/// takes the usual filter syntax and defaults to errors only.
fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::builder()
                .with_default_directive(tracing::Level::ERROR.into())
                .from_env_lossy(),
        )
        .with_writer(|| -> Box<dyn Write> {
            if QUIET.load(Ordering::Relaxed) {
                Box::new(std::io::sink())
            } else {
                Box::new(std::io::stderr())
            }
        })
        .init();
}

/// Set while a view holds the terminal, which diagnostics would draw over.
static QUIET: AtomicBool = AtomicBool::new(false);
