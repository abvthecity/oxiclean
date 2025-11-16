use anyhow::Result;
use clap::{Parser, Subcommand};
use colored::Colorize;
use log::{debug, info};
use oxiclean_import_bloat::Config;
use std::io::{BufWriter, Write};
use std::time::Instant;

#[derive(Parser)]
#[command(name = "oxiclean")]
#[command(about = "A collection of tools for cleaning up codebases", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Check for import bloat in JavaScript/TypeScript projects
    ImportBloat(Config),
}

fn main() -> Result<()> {
    env_logger::init();

    // stdio is blocked by LineWriter, use a BufWriter to reduce syscalls.
    // See https://github.com/rust-lang/rust/issues/60673
    let mut stdout = BufWriter::new(std::io::stdout());

    let cli = Cli::parse();
    debug!("Parsed CLI arguments: {:?}", cli.command);

    let start = Instant::now();

    match cli.command {
        Commands::ImportBloat(cfg) => {
            let num_threads = rayon::current_num_threads();
            info!(
                "Running import bloat check with threshold: {} (using {} threads)",
                cfg.threshold, num_threads
            );
            debug!("Config: root={:?}, entry_glob={:?}", cfg.root, cfg.entry_glob);

            let result = oxiclean_import_bloat::run_import_bloat_check(cfg.clone())?;
            debug!("Found {} warnings", result.warnings.len());

            let elapsed_ms = start.elapsed().as_millis();

            if !result.warnings.is_empty() {
                oxiclean_import_bloat::print_warnings_tree(
                    &mut stdout,
                    &result.warnings,
                    &cfg,
                    cfg.threshold,
                )?;

                writeln!(
                    stdout,
                    "\n{} Finished in {}ms on {} files (using {} threads).",
                    "●".bright_blue(),
                    elapsed_ms.to_string().cyan(),
                    result.files_analyzed.to_string().cyan(),
                    num_threads.to_string().cyan()
                )?;
                stdout.flush()?;

                // Non-zero exit to fail CI
                std::process::exit(1);
            } else {
                info!("No bloat detected");
                oxiclean_import_bloat::print_no_bloat_message(&mut stdout, cfg.threshold)?;
                writeln!(
                    stdout,
                    "\n{} Finished in {}ms on {} files (using {} threads).",
                    "●".bright_blue(),
                    elapsed_ms.to_string().cyan(),
                    result.files_analyzed.to_string().cyan(),
                    num_threads.to_string().cyan()
                )?;
                stdout.flush()?;
            }

            Ok(())
        }
    }
}
