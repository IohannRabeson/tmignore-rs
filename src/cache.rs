use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use crate::diff::Diff;

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

        Ok(Cache {
            paths,
            file_path: file_path.to_path_buf(),
        })
    }

    pub fn reset(&mut self, iter: impl IntoIterator<Item = PathBuf>) {
        self.paths = BTreeSet::from_iter(iter);
    }

    pub fn add_paths(&mut self, iter: impl Iterator<Item = PathBuf>) {
        for path in iter {
            self.paths.insert(path);
        }
    }

    pub fn remove_paths_in_directory(&mut self, directory: impl AsRef<Path>) {
        let directory = directory.as_ref();
        self.paths.retain(|path|{
            !path.starts_with(directory)
        });
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

    pub fn find_diff_in_directory(
        &self,
        exclusions: &BTreeSet<PathBuf>,
        directory: impl AsRef<Path>,
    ) -> Diff {
        let directory = directory.as_ref();
        let mut diff = Diff::default();
        let added = exclusions.difference(&self.paths);
        let removed = self.paths.difference(exclusions);
        for item in added.filter(|path| path.starts_with(directory)) {
            diff.added.insert(item.clone());
        }
        for item in removed.filter(|path| path.starts_with(directory)) {
            diff.removed.insert(item.clone());
        }
        diff
    }

    pub fn paths(&self) -> impl Iterator<Item = &Path> {
        self.paths.iter().map(|path| path.as_path())
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
        cache.reset([PathBuf::from("hello"), PathBuf::from("world")]);
        let exclusions = BTreeSet::from([PathBuf::from("world"), PathBuf::from("hey")]);
        let diff = cache.find_diff(&exclusions);
        assert_eq!(1, diff.added.len());
        assert!(diff.added.contains(&PathBuf::from("hey")));
        assert_eq!(1, diff.removed.len());
        assert!(diff.removed.contains(&PathBuf::from("hello")));
    }

    #[test]
    fn test_remove_paths_in_directory() {
        let mut cache = Cache {
            file_path: "".into(),
            paths: BTreeSet::from([PathBuf::from("hello").join("removed"), PathBuf::from("world")]),
        };

        cache.remove_paths_in_directory("hello");

        assert_eq!(1, cache.paths.len());
        assert_eq!(Some(&PathBuf::from("world")), cache.paths.first());
    }
}
