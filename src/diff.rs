use std::{collections::BTreeSet, path::PathBuf};

#[derive(Default)]
pub struct Diff {
    pub added: BTreeSet<PathBuf>,
    pub removed: BTreeSet<PathBuf>,
}
