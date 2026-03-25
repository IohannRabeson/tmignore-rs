use std::{
    cell::RefCell,
    collections::BTreeSet,
    ffi::OsStr,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
};

use anyhow::anyhow;
use log::info;
use rusqlite::{Connection, params};

use crate::diff::Diff;

/// The cache stores the list of paths to exclude from Time Machine backup.
/// I refer to it by "the exclusion list" in the public documentation.
#[derive(Debug)]
pub struct Cache {
    connection: RefCell<Connection>,
}

#[derive(thiserror::Error, Debug)]
pub enum OpenOrCreateError {
    #[error("File does not exist")]
    FileDoesNotExist,
    #[error("No parent directory")]
    NoParentDirectory,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

fn path_to_bytes(path: &Path) -> &[u8] {
    path.as_os_str().as_bytes()
}

impl Cache {
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let file_path = path.as_ref();
        Ok(match Self::load_from_file(file_path) {
            Ok(cache) => cache,
            Err(OpenOrCreateError::FileDoesNotExist) => Self::create(file_path)?,
            Err(error) => return Err(anyhow!("Failed to load file {}: {}", file_path.display(), error)),
        })
    }

    pub fn create(file_path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let file_path = file_path.as_ref();

        std::fs::create_dir_all(
            file_path
                .parent()
                .ok_or(OpenOrCreateError::NoParentDirectory)?,
        )?;

        if file_path.is_file() {
            std::fs::remove_file(file_path)?;
        }

        let mut cache = Self {
            connection: RefCell::new(Connection::open(file_path)?),
        };

        cache.setup()?;

        Ok(cache)
    }

    pub fn load_from_file(file_path: impl AsRef<Path>) -> Result<Self, OpenOrCreateError> {
        let file_path = file_path.as_ref();

        info!("Load cache '{}'", file_path.display());

        if !file_path.is_file() {
            return Err(OpenOrCreateError::FileDoesNotExist.into());
        }

        Ok(Self {
            connection: RefCell::new(Connection::open(file_path)?),
        })
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self, OpenOrCreateError> {
        let mut cache = Self {
            connection: RefCell::new(Connection::open_in_memory()?),
        };

        cache.setup()?;

        Ok(cache)
    }

    fn setup(&mut self) -> Result<(), OpenOrCreateError> {
        self.connection
            .borrow()
            .execute_batch(include_str!("sql/schema.sql"))?;
        Ok(())
    }

    const SQL_INSERT_PATH: &str = "INSERT INTO paths (path) VALUES (?)";

    pub fn reset(&mut self, iter: impl IntoIterator<Item = PathBuf>) {
        if let Ok(transaction) = self.connection.borrow_mut().transaction() {
            let mut insert_stmt = transaction.prepare(Self::SQL_INSERT_PATH).unwrap();
            transaction.execute("DELETE FROM paths", params![]).unwrap();
            for path in iter {
                insert_stmt.execute(params![path_to_bytes(&path)]).unwrap();
            }
            drop(insert_stmt);
            transaction.commit().unwrap();
        }
    }

    pub fn add_paths(&mut self, iter: impl Iterator<Item = PathBuf>) {
        if let Ok(transaction) = self.connection.borrow_mut().transaction() {
            let mut insert_stmt = transaction.prepare(Self::SQL_INSERT_PATH).unwrap();
            for path in iter {
                insert_stmt.execute(params![path_to_bytes(&path)]).unwrap();
            }
            drop(insert_stmt);
            transaction.commit().unwrap();
        }
    }

    pub fn remove_paths_in_directory(&mut self, directory: impl AsRef<Path>) {
        let directory = directory.as_ref();

        self.connection
            .borrow()
            .execute(
                "DELETE FROM paths WHERE path LIKE ? || '%'",
                params![path_to_bytes(directory)],
            )
            .unwrap();
    }

    pub fn find_diff(&self, exclusions: &BTreeSet<PathBuf>) -> Diff {
        let mut diff = Diff::default();

        {
            let connection = self.connection.borrow();
            let mut stmt = connection
                .prepare("SELECT * FROM paths WHERE path = ?")
                .unwrap();
            for exclusion in exclusions {
                if !stmt.exists(params![path_to_bytes(exclusion)]).unwrap() {
                    diff.added.insert(exclusion.clone());
                }
            }
        }

        {
            let connection = self.connection.borrow();
            let mut select_stmt = connection.prepare("SELECT path FROM paths").unwrap();
            let paths = select_stmt
                .query_map(params![], |row| {
                    let bytes: Vec<u8> = row.get(0).unwrap();

                    Ok(PathBuf::from(OsStr::from_bytes(&bytes)))
                })
                .unwrap();

            for path in paths.into_iter().filter_map(Result::ok) {
                if !exclusions.contains(&path) {
                    diff.removed.insert(path.clone());
                }
            }
        }

        diff
    }

    pub fn find_diff_in_directory(
        &self,
        exclusions: &BTreeSet<PathBuf>,
        directory: impl AsRef<Path>,
    ) -> Diff {
        let mut diff = Diff::default();
        let directory = directory.as_ref();
        {
            let connection = self.connection.borrow();
            let mut stmt = connection
                .prepare("SELECT * FROM paths WHERE path = ?")
                .unwrap();
            for exclusion in exclusions.iter().filter(|path| path.starts_with(directory)) {
                if !stmt.exists(params![path_to_bytes(exclusion)]).unwrap() {
                    diff.added.insert(exclusion.clone());
                }
            }
        }

        {
            let connection = self.connection.borrow();
            let mut select_stmt = connection
                .prepare("SELECT path FROM paths WHERE path LIKE ? || '%'")
                .unwrap();
            let paths = select_stmt
                .query_map(params![path_to_bytes(directory)], |row| {
                    let bytes: Vec<u8> = row.get(0).unwrap();

                    Ok(PathBuf::from(OsStr::from_bytes(&bytes)))
                })
                .unwrap();

            for path in paths.into_iter().filter_map(Result::ok) {
                if !exclusions.contains(&path) {
                    diff.removed.insert(path.clone());
                }
            }
        }

        diff
    }

    pub fn paths(&self) -> Vec<PathBuf> {
        let connection = self.connection.borrow();
        let mut stmt = connection.prepare("SELECT path FROM paths").unwrap();
        let paths = stmt
            .query_map(params![], |row| {
                let bytes: Vec<u8> = row.get(0).unwrap();

                Ok(PathBuf::from(OsStr::from_bytes(&bytes)))
            })
            .unwrap();

        paths.into_iter().filter_map(Result::ok).collect()
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, path::PathBuf};

    use temp_dir_builder::TempDirectoryBuilder;

    use crate::cache::OpenOrCreateError;

    use super::Cache;

    #[test]
    fn test_setup() {
        let _cache = Cache::open_in_memory().unwrap();
    }

    #[test]
    fn test_reset() {
        let mut cache = Cache::open_in_memory().unwrap();
        assert!(cache.paths().is_empty());
        cache.reset([PathBuf::from("hello"), PathBuf::from("world")]);
        assert_eq!(2, cache.paths().len());
    }

    #[test]
    fn test_find_diff() {
        let mut cache = Cache::open_in_memory().unwrap();
        cache.reset([PathBuf::from("hello"), PathBuf::from("world")]);
        let exclusions = BTreeSet::from([PathBuf::from("world"), PathBuf::from("hey")]);
        let diff = cache.find_diff(&exclusions);
        assert_eq!(1, diff.added.len());
        assert!(diff.added.contains(&PathBuf::from("hey")));
        assert_eq!(1, diff.removed.len());
        assert!(diff.removed.contains(&PathBuf::from("hello")));
    }

    #[test]
    fn test_find_diff_in_directory() {
        let mut cache = Cache::open_in_memory().unwrap();
        cache.reset([
            PathBuf::from("hello"),
            PathBuf::from("world"),
            PathBuf::from("1").join("a"),
            PathBuf::from("1").join("b"),
            PathBuf::from("1").join("c"),
        ]);
        let exclusions = BTreeSet::from([
            PathBuf::from("1").join("a"),
            PathBuf::from("1").join("b"),
            PathBuf::from("1").join("c"),
        ]);
        let diff = cache.find_diff_in_directory(&exclusions, PathBuf::from("1"));
        assert!(diff.added.is_empty());
        assert!(diff.removed.is_empty());
        let exclusions = BTreeSet::from([
            PathBuf::from("1").join("a"),
            PathBuf::from("1").join("b"),
            PathBuf::from("1").join("D"),
        ]);
        let diff = cache.find_diff_in_directory(&exclusions, PathBuf::from("1"));
        assert_eq!(1, diff.added.len());
        assert!(diff.added.contains(&PathBuf::from("1").join("D")));
        assert_eq!(1, diff.removed.len());
        assert!(diff.removed.contains(&PathBuf::from("1").join("c")));
    }

    #[test]
    fn test_remove_paths_in_directory() {
        let mut cache = Cache::open_in_memory().unwrap();

        cache.reset([
            PathBuf::from("hello").join("removed"),
            PathBuf::from("world"),
        ]);
        cache.remove_paths_in_directory("hello");

        assert_eq!(1, cache.paths().len());
        assert_eq!(Some(&PathBuf::from("world")), cache.paths().first());
    }

    #[test]
    fn test_open_cache_no_parent_dir() {
        let result = Cache::open("/");
        let err = result.unwrap_err();

        assert!(matches!(err.downcast(), Ok(OpenOrCreateError::NoParentDirectory)));
    }

    #[test]
    fn test_open_cache_create_no_legacy() {
        let temp_dir = TempDirectoryBuilder::default().build().unwrap();
        let cache_file_path = temp_dir.path().join("cache.db");
        let result = Cache::open(cache_file_path).unwrap();

        assert!(result.paths().is_empty());
    }

    #[test]
    fn test_open_cache_existing() {
        let temp_dir = TempDirectoryBuilder::default().build().unwrap();
        let cache_file_path = temp_dir.path().join("cache.db");
        {
            let mut cache = Cache::open(&cache_file_path).unwrap();
            cache.add_paths([PathBuf::from("yo")].into_iter());
        }
        let cache = Cache::open(&cache_file_path).unwrap();
        let paths = cache.paths();
        assert_eq!(1, paths.len());
        assert_eq!(PathBuf::from("yo"), paths[0]);
    }
}
