use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Exclusion {
    path: PathBuf,
    repository: Option<PathBuf>,
}

impl Exclusion {
    pub fn new(path: PathBuf, repository: PathBuf) -> Self {
        Self {
            path,
            repository: Some(repository),
        }
    }

    pub fn orphan(path: PathBuf) -> Self {
        Self {
            path,
            repository: None,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn repository(&self) -> Option<&Path> {
        self.repository.as_deref()
    }
}

#[derive(Default)]
pub struct Diff {
    pub added: BTreeSet<PathBuf>,
    pub removed: BTreeSet<PathBuf>,
}

impl Diff {
    /// `current` and `previous` must already be sorted by path: this uses `binary_search` against
    /// them.
    pub fn from_sorted(current: &[Exclusion], previous: &[PathBuf]) -> Self {
        debug_assert!(
            current.is_sorted_by_key(Exclusion::path),
            "`current` must be sorted by path"
        );
        debug_assert!(previous.is_sorted(), "`previous` must be sorted");

        Self {
            added: current
                .iter()
                .filter(|exclusion| {
                    previous
                        .binary_search_by(|path| path.as_path().cmp(exclusion.path()))
                        .is_err()
                })
                .map(|exclusion| exclusion.path().to_path_buf())
                .collect(),
            removed: previous
                .iter()
                .filter(|path| {
                    current
                        .binary_search_by(|exclusion| exclusion.path().cmp(path))
                        .is_err()
                })
                .cloned()
                .collect(),
        }
    }
}
