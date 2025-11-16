#[derive(Debug, Clone)]
pub struct Warning {
    pub import_statement: String,
    pub from_file: String,
    pub reachable_unique_modules: usize,
    /// The resolved file path (with extension) for relative imports, None for non-relative imports
    pub resolved_path: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CheckResult {
    pub warnings: Vec<Warning>,
    pub files_analyzed: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct Specifier {
    pub(crate) request: String,
    #[allow(dead_code)]
    pub(crate) kind: SpecKind,
}

#[derive(Debug, Clone)]
pub(crate) enum SpecKind {
    Static,
    Dynamic,
}
