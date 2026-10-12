use anyhow::{Result, anyhow};
use clap::Parser;
use log::{debug, info};
use oxiclean_core::Project;
use std::{path::PathBuf, sync::Arc};

#[derive(Debug, Clone, Parser)]
#[command(name = "import-depth")]
#[command(about = "Check for excessive import depth in JavaScript/TypeScript projects")]
pub struct Config {
    /// Root directory of the project (defaults to git root)
    #[arg(long)]
    pub root: Option<PathBuf>,

    /// Threshold for maximum import depth
    #[arg(long, default_value = "10")]
    pub threshold: usize,

    /// Glob pattern to filter entry files
    #[arg(long)]
    pub entry_glob: Option<String>,

    /// Resolution context, populated by `initialize()`
    #[clap(skip)]
    pub project: Arc<Project>,
}

impl Config {
    /// Initialize the config by resolving the root directory and loading tsconfig paths
    pub fn initialize(&mut self) -> Result<()> {
        // Resolve root directory
        let root = if let Some(r) = self.root.take() {
            debug!("Using provided root directory: {:?}", r);
            r.canonicalize().unwrap_or(r)
        } else {
            debug!("No root provided, searching for git root");
            oxiclean_core::find_git_root()?
        };
        info!("Using root directory: {}", root.display());

        // Discover tsconfig files and workspace packages
        debug!("Loading project context");
        self.project = Arc::new(Project::load(&root));
        debug!("Found {} workspace packages", self.project.workspace_package_count());

        self.root = Some(root);
        Ok(())
    }

    /// Get the root directory, returning an error if not initialized
    pub fn root(&self) -> Result<&PathBuf> {
        self.root
            .as_ref()
            .ok_or_else(|| anyhow!("Config not initialized - call initialize() first"))
    }
}
