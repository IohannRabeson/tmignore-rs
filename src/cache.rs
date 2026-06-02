use std::{
    cell::RefCell,
    collections::BTreeSet,
    ffi::OsStr,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
};

use anyhow::anyhow;
use chrono::{DateTime, Utc};
use log::{debug, info};
use rusqlite::{Connection, Row, Transaction, params};
use rusqlite_migration::{M, Migrations};

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

const MIGRATIONS_SLICE: &[M<'_>] = &[
    M::up(include_str!("sql/v0.sql")),
    M::up(include_str!("sql/v1.sql")),
];
const MIGRATIONS: Migrations<'_> = Migrations::from_slice(MIGRATIONS_SLICE);

impl Cache {
    /// Open or create a `Cache` and setup or update the schema.
    pub fn open_or_create(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let file_path = path.as_ref();

        Ok(match Self::open(file_path) {
            Ok(cache) => cache,
            Err(error) => {
                if let Some(OpenOrCreateError::FileDoesNotExist) =
                    error.downcast_ref::<OpenOrCreateError>()
                {
                    Self::create(file_path)?
                } else {
                    let message = format!("Failed to load file {}", file_path.display());

                    return Err(error.context(message));
                }
            }
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

    /// Load a `Cache` by reading a file.
    /// It is public only for testing purpose only, use `Cache::open`
    pub fn open(file_path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let file_path = file_path.as_ref();

        debug!("Open cache '{}'", file_path.display());

        if !file_path.is_file() {
            return Err(OpenOrCreateError::FileDoesNotExist.into());
        }

        let mut cache = Self {
            connection: RefCell::new(Connection::open(file_path)?),
        };

        let previous_version = cache.get_version()?;
        cache.setup()?;
        let new_version = cache.get_version()?;
        if previous_version != new_version {
            info!("Cache updated from version {previous_version} to version {new_version}");
        }
        Ok(cache)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> anyhow::Result<Self> {
        let mut cache = Self {
            connection: RefCell::new(Connection::open_in_memory()?),
        };

        cache.setup()?;

        Ok(cache)
    }

    fn setup(&mut self) -> anyhow::Result<()> {
        MIGRATIONS.to_latest(&mut self.connection.borrow_mut())?;
        Self::set_last_update_connection(&mut self.connection.borrow_mut())?;
        Ok(())
    }

    const SQL_INSERT_PATH: &str = "INSERT INTO paths (path) VALUES (?)";
    const SQL_SET_LAST_UPDATE: &str = "UPDATE metadata SET last_update=?";

    pub fn reset(&mut self, iter: impl IntoIterator<Item = PathBuf>) -> anyhow::Result<()> {
        let mut connection = self.connection.borrow_mut();
        let mut transaction = connection.transaction()?;
        let mut insert_stmt = transaction.prepare(Self::SQL_INSERT_PATH)?;
        transaction.execute("DELETE FROM paths", params![])?;
        for path in iter {
            insert_stmt.execute(params![path_to_bytes(&path)])?;
        }
        drop(insert_stmt);
        Self::set_last_update_transaction(&mut transaction)?;
        transaction.commit()?;
        Ok(())
    }

    fn set_last_update_transaction(transaction: &mut Transaction) -> anyhow::Result<()> {
        let now = chrono::Utc::now();

        transaction.execute(Self::SQL_SET_LAST_UPDATE, params![now])?;

        Ok(())
    }

    fn set_last_update_connection(connection: &mut Connection) -> anyhow::Result<()> {
        let now = chrono::Utc::now();

        connection.execute(Self::SQL_SET_LAST_UPDATE, params![now])?;

        Ok(())
    }

    pub fn add_paths(&mut self, iter: impl Iterator<Item = PathBuf>) -> anyhow::Result<()> {
        let mut connection = self.connection.borrow_mut();
        let mut transaction = connection.transaction()?;
        let mut insert_stmt = transaction.prepare(Self::SQL_INSERT_PATH)?;
        for path in iter {
            insert_stmt.execute(params![path_to_bytes(&path)])?;
        }
        drop(insert_stmt);
        Self::set_last_update_transaction(&mut transaction)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn remove_paths_in_directory(&mut self, directory: impl AsRef<Path>) -> anyhow::Result<()> {
        let directory = directory.as_ref();
        let mut connection = self.connection.borrow_mut();
        let mut transaction = connection.transaction()?;
        transaction.execute(
            "DELETE FROM paths WHERE path = ? OR path LIKE ? || '/%'",
            params![path_to_bytes(directory), path_to_bytes(directory)],
        )?;
        Self::set_last_update_transaction(&mut transaction)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn find_diff(&self, exclusions: &BTreeSet<PathBuf>) -> anyhow::Result<Diff> {
        let mut diff = Diff::default();

        {
            let connection = self.connection.borrow();
            let mut stmt = connection.prepare("SELECT * FROM paths WHERE path = ?")?;
            for exclusion in exclusions {
                if !stmt.exists(params![path_to_bytes(exclusion)])? {
                    diff.added.insert(exclusion.clone());
                }
            }
        }

        {
            let connection = self.connection.borrow();
            let mut select_stmt = connection.prepare("SELECT path FROM paths")?;
            let paths = select_stmt.query_map(params![], |row| {
                let bytes: Vec<u8> = row.get(0)?;

                Ok(PathBuf::from(OsStr::from_bytes(&bytes)))
            })?;

            for path in paths.into_iter().filter_map(Result::ok) {
                if !exclusions.contains(&path) {
                    diff.removed.insert(path.clone());
                }
            }
        }

        Ok(diff)
    }

    pub fn find_diff_in_directory(
        &self,
        exclusions: &BTreeSet<PathBuf>,
        directory: impl AsRef<Path>,
    ) -> anyhow::Result<Diff> {
        let mut diff = Diff::default();
        let directory = directory.as_ref();
        {
            let connection = self.connection.borrow();
            let mut stmt = connection.prepare("SELECT * FROM paths WHERE path = ?")?;
            for exclusion in exclusions.iter().filter(|path| path.starts_with(directory)) {
                if !stmt.exists(params![path_to_bytes(exclusion)])? {
                    diff.added.insert(exclusion.clone());
                }
            }
        }

        {
            let connection = self.connection.borrow();
            let mut select_stmt = connection
                .prepare("SELECT path FROM paths WHERE path = ? OR path LIKE ? || '/%'")?;
            let paths = select_stmt.query_map(
                params![path_to_bytes(directory), path_to_bytes(directory)],
                |row| {
                    let bytes: Vec<u8> = row.get(0)?;

                    Ok(PathBuf::from(OsStr::from_bytes(&bytes)))
                },
            )?;

            for path in paths.into_iter().filter_map(Result::ok) {
                if !exclusions.contains(&path) {
                    diff.removed.insert(path.clone());
                }
            }
        }

        Ok(diff)
    }

    pub fn paths(&self) -> anyhow::Result<Vec<PathBuf>> {
        let connection = self.connection.borrow();
        let mut stmt = connection.prepare("SELECT path FROM paths")?;
        let paths = stmt.query_map(params![], |row| {
            let bytes: Vec<u8> = row.get(0)?;

            Ok(PathBuf::from(OsStr::from_bytes(&bytes)))
        })?;

        Ok(paths.into_iter().filter_map(Result::ok).collect())
    }

    /// Get the date and time of the last cache update.
    ///
    /// This function will panic for a cache with the schema V0 but that
    /// supposed to never happen.
    pub fn last_update(&self) -> anyhow::Result<DateTime<Utc>> {
        if self.get_version()? == 0 {
            return Err(anyhow!(
                "last_update is only available for schema with version > 0"
            ));
        }

        Ok(self.connection.borrow().query_one(
            "SELECT last_update FROM metadata WHERE id = 0",
            params![],
            |row: &Row<'_>| row.get(0),
        )?)
    }

    pub fn get_version(&self) -> anyhow::Result<u32> {
        Ok(self
            .connection
            .borrow()
            .pragma_query_value(None, "user_version", |r| r.get(0))?)
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, path::PathBuf};

    use temp_dir_builder::TempDirectoryBuilder;

    use crate::cache::{MIGRATIONS_SLICE, OpenOrCreateError};

    use super::Cache;

    #[test]
    fn test_migrations() {
        assert!(super::MIGRATIONS.validate().is_ok());
    }

    #[test]
    fn test_setup() {
        let _cache = Cache::open_in_memory().unwrap();
    }

    #[test]
    fn test_last_update() {
        let cache = Cache::open_in_memory().unwrap();
        let _ = cache.last_update();
    }

    #[test]
    fn test_version() {
        let cache = Cache::open_in_memory().unwrap();
        let version = cache.get_version().unwrap();

        assert_eq!(MIGRATIONS_SLICE.len() as u32, version);
    }

    #[test]
    fn test_reset() {
        let mut cache = Cache::open_in_memory().unwrap();
        assert!(cache.paths().unwrap().is_empty());
        cache
            .reset([PathBuf::from("hello"), PathBuf::from("world")])
            .unwrap();
        assert_eq!(2, cache.paths().unwrap().len());
    }

    #[test]
    fn test_find_diff() {
        let mut cache = Cache::open_in_memory().unwrap();
        cache
            .reset([PathBuf::from("hello"), PathBuf::from("world")])
            .unwrap();
        let exclusions = BTreeSet::from([PathBuf::from("world"), PathBuf::from("hey")]);
        let diff = cache.find_diff(&exclusions).unwrap();
        assert_eq!(1, diff.added.len());
        assert!(diff.added.contains(&PathBuf::from("hey")));
        assert_eq!(1, diff.removed.len());
        assert!(diff.removed.contains(&PathBuf::from("hello")));
    }

    #[test]
    fn test_find_diff_in_directory() {
        let mut cache = Cache::open_in_memory().unwrap();
        cache
            .reset([
                PathBuf::from("hello"),
                PathBuf::from("world"),
                PathBuf::from("1").join("a"),
                PathBuf::from("1").join("b"),
                PathBuf::from("1").join("c"),
            ])
            .unwrap();
        let exclusions = BTreeSet::from([
            PathBuf::from("1").join("a"),
            PathBuf::from("1").join("b"),
            PathBuf::from("1").join("c"),
        ]);
        let diff = cache
            .find_diff_in_directory(&exclusions, PathBuf::from("1"))
            .unwrap();
        assert!(diff.added.is_empty());
        assert!(diff.removed.is_empty());
        let exclusions = BTreeSet::from([
            PathBuf::from("1").join("a"),
            PathBuf::from("1").join("b"),
            PathBuf::from("1").join("D"),
        ]);
        let diff = cache
            .find_diff_in_directory(&exclusions, PathBuf::from("1"))
            .unwrap();
        assert_eq!(1, diff.added.len());
        assert!(diff.added.contains(&PathBuf::from("1").join("D")));
        assert_eq!(1, diff.removed.len());
        assert!(diff.removed.contains(&PathBuf::from("1").join("c")));
    }

    #[test]
    fn test_find_diff_in_directory_does_not_affect_sibling_directory() {
        let mut cache = Cache::open_in_memory().unwrap();
        cache
            .reset([
                PathBuf::from("/repo/file"),
                PathBuf::from("/repo-sibling/file"),
            ])
            .unwrap();
        let exclusions = BTreeSet::from([PathBuf::from("/repo/file")]);
        let diff = cache
            .find_diff_in_directory(&exclusions, PathBuf::from("/repo"))
            .unwrap();

        assert!(
            diff.removed.is_empty(),
            "/repo-sibling/file was incorrectly included in the diff for /repo"
        );
        assert!(diff.added.is_empty());
    }

    #[test]
    fn test_remove_paths_in_directory() {
        let mut cache = Cache::open_in_memory().unwrap();

        cache
            .reset([
                PathBuf::from("hello").join("removed"),
                PathBuf::from("world"),
            ])
            .unwrap();
        cache.remove_paths_in_directory("hello").unwrap();

        assert_eq!(1, cache.paths().unwrap().len());
        assert_eq!(
            Some(&PathBuf::from("world")),
            cache.paths().unwrap().first()
        );
    }

    #[test]
    fn test_remove_paths_in_directory_does_not_affect_sibling_directory() {
        let mut cache = Cache::open_in_memory().unwrap();

        cache
            .reset([
                PathBuf::from("/repo/file"),
                PathBuf::from("/repo-sibling/file"),
            ])
            .unwrap();
        cache.remove_paths_in_directory("/repo").unwrap();

        let paths = cache.paths().unwrap();
        assert_eq!(1, paths.len());
        assert!(
            paths.contains(&PathBuf::from("/repo-sibling/file")),
            "/repo-sibling/file was incorrectly deleted when removing /repo"
        );
    }

    #[test]
    fn test_remove_paths_in_directory_like_wildcards_and_case() {
        let mut cache = Cache::open_in_memory().unwrap();

        cache
            .reset([
                PathBuf::from("/Users/me/my_project/file"),
                PathBuf::from("/Users/me/myXproject/file"),
                PathBuf::from("/Users/me/MY_PROJECT/file"),
            ])
            .unwrap();

        cache
            .remove_paths_in_directory("/Users/me/my_project")
            .unwrap();

        let paths = cache.paths().unwrap();
        assert!(
            paths.contains(&PathBuf::from("/Users/me/myXproject/file")),
            "'_' in the directory name was treated as a SQL LIKE wildcard, \
             so the unrelated sibling /Users/me/myXproject/file was deleted"
        );
        assert!(
            paths.contains(&PathBuf::from("/Users/me/MY_PROJECT/file")),
            "SQL LIKE matched case-insensitively, so /Users/me/MY_PROJECT/file was deleted"
        );
        assert_eq!(
            2,
            paths.len(),
            "only /Users/me/my_project/file should have been removed"
        );
    }

    #[test]
    fn test_open_cache_no_parent_dir() {
        let result = Cache::open_or_create("/");
        let err = result.unwrap_err();

        assert!(matches!(
            err.downcast(),
            Ok(OpenOrCreateError::NoParentDirectory)
        ));
    }

    #[test]
    fn test_open_cache_create_no_legacy() {
        let temp_dir = TempDirectoryBuilder::default().build().unwrap();
        let cache_file_path = temp_dir.path().join("cache.db");
        let result = Cache::open_or_create(cache_file_path).unwrap();

        assert!(result.paths().unwrap().is_empty());
    }

    #[test]
    fn test_open_cache_existing() {
        let temp_dir = TempDirectoryBuilder::default().build().unwrap();
        let cache_file_path = temp_dir.path().join("cache.db");
        {
            let mut cache = Cache::open_or_create(&cache_file_path).unwrap();
            cache.add_paths([PathBuf::from("yo")].into_iter()).unwrap();
        }
        let cache = Cache::open_or_create(&cache_file_path).unwrap();
        let paths = cache.paths().unwrap();
        assert_eq!(1, paths.len());
        assert_eq!(PathBuf::from("yo"), paths[0]);
    }
}
