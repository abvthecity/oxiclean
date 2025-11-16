#[derive(Debug, Clone)]
pub struct Warning {
    pub import_statement: String,
    pub from_file: String,
    pub depth: usize,
    /// The resolved file path (with extension) for relative imports, None for non-relative imports
    pub resolved_path: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CheckResult {
    pub warnings: Vec<Warning>,
    pub files_analyzed: usize,
}
