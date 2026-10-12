#[derive(Debug, Clone)]
pub struct Specifier {
    pub request: String,
    pub kind: SpecKind,
}

impl Specifier {
    /// Whether this edge is loaded eagerly. Dynamic `import()` creates a separate
    /// chunk, so it does not count toward an importer's bloat or depth.
    pub fn is_static(&self) -> bool {
        matches!(self.kind, SpecKind::Static)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecKind {
    Static,
    Dynamic,
}
