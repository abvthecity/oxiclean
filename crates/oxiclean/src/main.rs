use std::{
    io::{BufWriter, Write},
    process::ExitCode,
    time::Instant,
};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use colored::Colorize;
use oxiclean_core::{
    CommandResult, fix,
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
}

impl Commands {
    /// The command's name, as typed.
    fn name(&self) -> &'static str {
        match self {
            Self::Unused(_) => "unused",
            Self::Dependencies(_) => "dependencies",
            Self::Typecheck(_) => "typecheck",
            Self::Bloat(_) => "bloat",
            Self::Trace(_) => "trace",
            Self::Depth(_) => "depth",
            Self::Cycles(_) => "cycles",
            Self::Duplicates(_) => "duplicates",
        }
    }

    /// Runs each of the command's rules the config file leaves on.
    fn run(&self) -> oxiclean_core::Result<CommandResult> {
        match self {
            Self::Unused(cfg) => oxiclean_unused_exports::run_unused_command(cfg),
            Self::Dependencies(cfg) => oxiclean_dependencies::run_dependencies_command(cfg),
            Self::Typecheck(cfg) => oxiclean_typecheck::run_typecheck_command(cfg),
            Self::Bloat(cfg) => oxiclean_bloat::run_bloat_command(cfg),
            Self::Trace(cfg) => oxiclean_trace::run_trace_command(cfg),
            Self::Depth(cfg) => oxiclean_depth::run_depth_command(cfg),
            Self::Cycles(cfg) => oxiclean_cycles::run_cycles_command(cfg),
            Self::Duplicates(cfg) => oxiclean_duplicates::run_duplicates_command(cfg),
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
            report::print_command(&mut stdout, command, &result, elapsed_ms, threads, ctx)?;
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
        .with_writer(std::io::stderr)
        .init();
}
