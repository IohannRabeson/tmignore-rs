mod legacy_cache;

pub use legacy_cache::LegacyCache;

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

pub struct Cache {
    file_path: PathBuf,
    paths: BTreeSet<PathBuf>,
}

pub enum OpenOrCreate {
    Opened(Cache),
    Created(Cache),
}

#[derive(thiserror::Error, Debug)]
pub enum OpenOrCreateError {
    #[error("File does not exist")]
    FileDoesNotExist,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[derive(Default)]
pub struct Diff {
    pub added: BTreeSet<PathBuf>,
    pub removed: BTreeSet<PathBuf>,
}

impl Cache {
    pub fn open_or_create(path: impl AsRef<Path>) -> Result<OpenOrCreate, OpenOrCreateError> {
        let path = path.as_ref();
        Ok(match Self::load_from_file(path) {
            Ok(cache) => OpenOrCreate::Opened(cache),
            Err(OpenOrCreateError::FileDoesNotExist) => OpenOrCreate::Created(Self {
                file_path: path.to_path_buf(),
                paths: BTreeSet::new(),
            }),
            Err(error) => return Err(error),
        })
    }

    pub fn load_from_file(file_path: impl AsRef<Path>) -> Result<Cache, OpenOrCreateError> {
        let file_path = file_path.as_ref();

        if !file_path.is_file() {
            return Err(OpenOrCreateError::FileDoesNotExist);
        }

        let file = std::fs::File::open(file_path)?;
        let paths = serde_json::from_reader(file)?;

        Ok(Cache { paths, file_path: file_path.to_path_buf() })
    }

    pub fn write(&mut self, iter: impl IntoIterator<Item = PathBuf>) {
        self.paths = BTreeSet::from_iter(iter);
    }

    pub fn save_to_file(&self) -> Result<(), std::io::Error> {
        let file = std::fs::File::create(&self.file_path)?;

        serde_json::to_writer(file, &self.paths)?;

        Ok(())
    }

    pub fn find_diff(&self, exclusions: &BTreeSet<PathBuf>) -> Diff {
        let mut diff = Diff::default();
        let added = exclusions.difference(&self.paths);
        let removed = self.paths.difference(exclusions);
        for item in added {
            diff.added.insert(item.clone());
        }
        for item in removed {
            diff.removed.insert(item.clone());
        }
        diff
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, path::PathBuf};

    use crate::cache::Cache;

    #[test]
    fn test_find_diff() {
        let mut cache = Cache {
            file_path: "".into(),
            paths: BTreeSet::new(),
        };
        cache.write([PathBuf::from("hello"), PathBuf::from("world")]);
        let exclusions = BTreeSet::from([PathBuf::from("world"), PathBuf::from("hey")]);
        let diff = cache.find_diff(&exclusions);
        assert_eq!(1, diff.added.len());
        assert!(diff.added.contains(&PathBuf::from("hey")));
        assert_eq!(1, diff.removed.len());
        assert!(diff.removed.contains(&PathBuf::from("hello")));
    }
}
