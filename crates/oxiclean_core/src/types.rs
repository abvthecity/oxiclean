#[derive(Debug, Clone)]
pub struct Specifier {
    pub request: String,
    #[allow(dead_code)]
    pub kind: SpecKind,
}

#[derive(Debug, Clone)]
pub enum SpecKind {
    Static,
    Dynamic,
}
