//! Line numbers of byte offsets in a source file.

/// Where each line of a source file starts.
pub(crate) struct LineIndex {
    starts: Vec<usize>,
}

impl LineIndex {
    pub(crate) fn new(src: &str) -> Self {
        Self {
            starts: std::iter::once(0).chain(src.match_indices('\n').map(|(i, _)| i + 1)).collect(),
        }
    }

    /// The 1-based line `offset` is on.
    pub(crate) fn line(&self, offset: usize) -> u32 {
        u32::try_from(self.starts.partition_point(|&s| s <= offset)).unwrap_or(u32::MAX)
    }
}
