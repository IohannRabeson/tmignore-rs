use std::{
    cell::RefCell,
    collections::BTreeSet,
    ffi::OsStr,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
};

use rusqlite::{Connection, params};

use crate::diff::Diff;

pub struct Cache {
    connection: RefCell<Connection>,
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
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

fn path_to_bytes<'a>(path: &'a Path) -> &'a [u8] {
    path.as_os_str().as_bytes()
}

impl Cache {
    pub fn open_or_create(path: impl AsRef<Path>) -> Result<OpenOrCreate, OpenOrCreateError> {
        let path = path.as_ref();
        Ok(match Self::load_from_file(path) {
            Ok(cache) => OpenOrCreate::Opened(cache),
            Err(OpenOrCreateError::FileDoesNotExist) => {
                let mut cache = Self {
                    connection: RefCell::new(Connection::open(path)?),
                };

                cache.setup()?;

                OpenOrCreate::Created(cache)
            }
            Err(error) => return Err(error),
        })
    }

    fn setup(&mut self) -> Result<(), OpenOrCreateError> {
        self.connection
            .borrow()
            .execute_batch(include_str!("sql/schema.sql"))?;
        Ok(())
    }

    pub fn load_from_file(file_path: impl AsRef<Path>) -> Result<Cache, OpenOrCreateError> {
        let file_path = file_path.as_ref();

        if !file_path.is_file() {
            return Err(OpenOrCreateError::FileDoesNotExist);
        }

        Ok(Self {
            connection: RefCell::new(Connection::open(file_path)?),
        })
    }

    #[cfg(test)]
    fn open_in_memory() -> Result<Self, OpenOrCreateError> {
        let mut cache = Self {
            connection: RefCell::new(Connection::open_in_memory()?),
        };

        cache.setup()?;

        Ok(cache)
    }

    const SQL_INSERT_PATH: &str = "INSERT INTO paths (path) VALUES (?)";

    pub fn reset(&mut self, iter: impl IntoIterator<Item = PathBuf>) {
        if let Ok(transaction) = self.connection.borrow_mut().transaction() {
            {
                let mut insert_stmt = transaction.prepare(Self::SQL_INSERT_PATH).unwrap();
                transaction.execute("DELETE FROM paths", params![]).unwrap();
                for path in iter {
                    insert_stmt.execute(params![path_to_bytes(&path)]).unwrap();
                }
            }
            transaction.commit().unwrap();
        }
    }

    pub fn add_paths(&mut self, iter: impl Iterator<Item = PathBuf>) {
        if let Ok(transaction) = self.connection.borrow_mut().transaction() {
            {
                let mut insert_stmt = transaction.prepare(Self::SQL_INSERT_PATH).unwrap();
                for path in iter {
                    insert_stmt.execute(params![path_to_bytes(&path)]).unwrap();
                }
            }
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

            for path in paths.into_iter().filter_map(|path| path.ok()) {
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

            for path in paths.into_iter().filter_map(|path| path.ok()) {
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

        paths.into_iter().filter_map(|path| path.ok()).collect()
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, path::PathBuf};

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
}
