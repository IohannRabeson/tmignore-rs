use std::{collections::BTreeSet, path::PathBuf};

#[derive(Default)]
pub struct Diff {
    pub added: BTreeSet<PathBuf>,
    pub removed: BTreeSet<PathBuf>,
}

impl Diff {
    /// `current` and `previous` must already be sorted: this uses `binary_search` against them.
    pub fn from_sorted(current: &[PathBuf], previous: &[PathBuf]) -> Self {
        Self {
            added: current
                .iter()
                .filter(|path| previous.binary_search(path).is_err())
                .cloned()
                .collect(),
            removed: previous
                .iter()
                .filter(|path| current.binary_search(path).is_err())
                .cloned()
                .collect(),
        }
    }
}
