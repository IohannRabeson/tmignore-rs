use std::{
    cell::RefCell,
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

struct DirectoryPrefixBounds {
    exact: Vec<u8>,
    lower: Vec<u8>,
    upper: Vec<u8>,
}

impl DirectoryPrefixBounds {
    fn new(directory: &Path) -> Self {
        let exact = path_to_bytes(directory).to_vec();
        let mut lower = exact.clone();
        lower.push(b'/');
        let mut upper = exact.clone();
        upper.push(b'/' + 1);
        Self {
            exact,
            lower,
            upper,
        }
    }
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

    pub fn remove_paths<'a>(
        &mut self,
        paths: impl Iterator<Item = &'a PathBuf>,
    ) -> anyhow::Result<()> {
        let mut connection = self.connection.borrow_mut();
        let mut transaction = connection.transaction()?;
        {
            let mut delete_stmt = transaction.prepare("DELETE FROM paths WHERE path = ?")?;
            for path in paths {
                delete_stmt.execute(params![path_to_bytes(path)])?;
            }
        }
        Self::set_last_update_transaction(&mut transaction)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn paths_with_prefix(&self, directory: impl AsRef<Path>) -> anyhow::Result<Vec<PathBuf>> {
        let directory = directory.as_ref();
        let bounds = DirectoryPrefixBounds::new(directory);
        let connection = self.connection.borrow();
        let mut select_stmt = connection
            .prepare("SELECT path FROM paths WHERE path = ?1 OR (path >= ?2 AND path < ?3)")?;
        let paths =
            select_stmt.query_map(params![bounds.exact, bounds.lower, bounds.upper], |row| {
                let bytes: Vec<u8> = row.get(0)?;

                Ok(PathBuf::from(OsStr::from_bytes(&bytes)))
            })?;

        Ok(paths.filter_map(Result::ok).collect())
    }

    /// `exclusions` must already be sorted: this uses `binary_search` against it.
    pub fn find_diff(&self, exclusions: &[PathBuf]) -> anyhow::Result<Diff> {
        let connection = self.connection.borrow();
        let mut select_stmt = connection.prepare("SELECT path FROM paths")?;
        let mut cached_paths: Vec<PathBuf> = select_stmt
            .query_map(params![], |row| {
                let bytes: Vec<u8> = row.get(0)?;

                Ok(PathBuf::from(OsStr::from_bytes(&bytes)))
            })?
            .filter_map(Result::ok)
            .collect();
        cached_paths.sort_unstable();

        Ok(Diff::from_sorted(exclusions, &cached_paths))
    }

    pub fn contains_ancestor_of(&self, path: impl AsRef<Path>) -> anyhow::Result<bool> {
        let connection = self.connection.borrow();
        let mut stmt = connection.prepare("SELECT * FROM paths WHERE path = ?1 OR path = ?2")?;
        let mut current = path.as_ref().parent();

        while let Some(ancestor) = current {
            let exact = path_to_bytes(ancestor);
            let mut with_separator = exact.to_vec();
            with_separator.push(b'/');
            if stmt.exists(params![exact, with_separator])? {
                return Ok(true);
            }
            current = ancestor.parent();
        }

        Ok(false)
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
    pub fn last_update(&self) -> anyhow::Result<Option<DateTime<Utc>>> {
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
    use std::{assert_matches, collections::BTreeSet, path::PathBuf};

    use rstest::rstest;
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
    fn test_last_update_is_none_for_new_cache() {
        let cache = Cache::open_in_memory().unwrap();
        assert!(cache.last_update().unwrap().is_none());
    }

    #[test]
    fn test_reset_sets_last_update() {
        let mut cache = Cache::open_in_memory().unwrap();
        assert!(cache.last_update().unwrap().is_none());

        cache.reset([PathBuf::from("hello")]).unwrap();
        let first_update = cache.last_update().unwrap().unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));
        cache.reset([PathBuf::from("world")]).unwrap();
        let second_update = cache.last_update().unwrap().unwrap();

        assert!(second_update > first_update);
    }

    #[test]
    fn test_add_paths_sets_last_update() {
        let mut cache = Cache::open_in_memory().unwrap();
        assert!(cache.last_update().unwrap().is_none());

        cache
            .add_paths([PathBuf::from("hello")].into_iter())
            .unwrap();
        let first_update = cache.last_update().unwrap().unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));
        cache
            .add_paths([PathBuf::from("world")].into_iter())
            .unwrap();
        let second_update = cache.last_update().unwrap().unwrap();

        assert!(second_update > first_update);
    }

    #[test]
    fn test_remove_paths_sets_last_update() {
        let mut cache = Cache::open_in_memory().unwrap();
        assert!(cache.last_update().unwrap().is_none());

        cache.remove_paths([PathBuf::from("hello")].iter()).unwrap();
        let first_update = cache.last_update().unwrap().unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));
        cache.remove_paths([PathBuf::from("world")].iter()).unwrap();
        let second_update = cache.last_update().unwrap().unwrap();

        assert!(second_update > first_update);
    }

    #[test]
    fn test_version() {
        let cache = Cache::open_in_memory().unwrap();
        let version = cache.get_version().unwrap();

        assert_eq!(u32::try_from(MIGRATIONS_SLICE.len()), Ok(version));
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
        let mut exclusions = vec![PathBuf::from("world"), PathBuf::from("hey")];
        exclusions.sort_unstable();
        let diff = cache.find_diff(&exclusions).unwrap();
        assert_eq!(1, diff.added.len());
        assert!(diff.added.contains(&PathBuf::from("hey")));
        assert_eq!(1, diff.removed.len());
        assert!(diff.removed.contains(&PathBuf::from("hello")));
    }

    #[test]
    fn test_paths_with_prefix() {
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

        let paths: BTreeSet<_> = cache
            .paths_with_prefix(PathBuf::from("1"))
            .unwrap()
            .into_iter()
            .collect();

        assert_eq!(
            BTreeSet::from([
                PathBuf::from("1").join("a"),
                PathBuf::from("1").join("b"),
                PathBuf::from("1").join("c"),
            ]),
            paths
        );
    }

    #[test]
    fn test_paths_with_prefix_does_not_include_sibling_directory() {
        let mut cache = Cache::open_in_memory().unwrap();
        cache
            .reset([
                PathBuf::from("/repo/file"),
                PathBuf::from("/repo-sibling/file"),
            ])
            .unwrap();

        let paths = cache.paths_with_prefix(PathBuf::from("/repo")).unwrap();

        assert_eq!(
            vec![PathBuf::from("/repo/file")],
            paths,
            "/repo-sibling/file was incorrectly included in the paths for /repo"
        );
    }

    #[test]
    fn test_paths_with_prefix_like_wildcards_and_case() {
        let mut cache = Cache::open_in_memory().unwrap();

        cache
            .reset([
                PathBuf::from("/Users/me/my_project/file"),
                PathBuf::from("/Users/me/myXproject/file"),
                PathBuf::from("/Users/me/MY_PROJECT/file"),
            ])
            .unwrap();

        let paths = cache
            .paths_with_prefix(PathBuf::from("/Users/me/my_project"))
            .unwrap();

        assert_eq!(
            vec![PathBuf::from("/Users/me/my_project/file")],
            paths,
            "'_' should not be treated as a SQL LIKE wildcard, and matching should be \
             case-sensitive"
        );
    }

    #[test]
    fn test_remove_paths() {
        let mut cache = Cache::open_in_memory().unwrap();

        cache
            .reset([
                PathBuf::from("hello").join("removed"),
                PathBuf::from("world"),
            ])
            .unwrap();
        cache
            .remove_paths([PathBuf::from("hello").join("removed")].iter())
            .unwrap();

        assert_eq!(1, cache.paths().unwrap().len());
        assert_eq!(
            Some(&PathBuf::from("world")),
            cache.paths().unwrap().first()
        );
    }

    #[test]
    fn test_remove_paths_does_not_affect_paths_not_listed() {
        let mut cache = Cache::open_in_memory().unwrap();

        cache
            .reset([
                PathBuf::from("/repo/file"),
                PathBuf::from("/repo-sibling/file"),
            ])
            .unwrap();
        cache
            .remove_paths([PathBuf::from("/repo/file")].iter())
            .unwrap();

        let paths = cache.paths().unwrap();
        assert_eq!(1, paths.len());
        assert!(
            paths.contains(&PathBuf::from("/repo-sibling/file")),
            "/repo-sibling/file was incorrectly deleted"
        );
    }

    #[rstest]
    #[case("/repo/target/debug/binary", true)]
    #[case("/repo/target/file", true)]
    #[case("/repo/target", false)]
    #[case("/repo/src/main.rs", false)]
    #[case("/repo-sibling/file", false)]
    #[case("/", false)]
    fn test_contains_ancestor_of(#[case] path: &str, #[case] expected: bool) {
        let mut cache = Cache::open_in_memory().unwrap();
        cache
            .reset([PathBuf::from("/repo/target"), PathBuf::from("/repo/a")])
            .unwrap();

        assert_eq!(expected, cache.contains_ancestor_of(path).unwrap());
    }

    #[rstest]
    #[case("/repo/target/debug/binary", true)]
    #[case("/repo/target/file", true)]
    #[case("/repo/target", false)]
    #[case("/repo/src/main.rs", false)]
    fn test_contains_ancestor_of_directory_stored_with_trailing_separator(
        #[case] path: &str,
        #[case] expected: bool,
    ) {
        let mut cache = Cache::open_in_memory().unwrap();
        cache.reset([PathBuf::from("/repo/target/")]).unwrap();

        assert_eq!(expected, cache.contains_ancestor_of(path).unwrap());
    }

    #[test]
    fn test_open_cache_no_parent_dir() {
        let result = Cache::open_or_create("/");
        let err = result.unwrap_err();

        assert_matches!(err.downcast(), Ok(OpenOrCreateError::NoParentDirectory));
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

    #[test]
    fn test_open_does_not_change_last_update() {
        let temp_dir = TempDirectoryBuilder::default().build().unwrap();
        let cache_file_path = temp_dir.path().join("cache.db");
        let last_update_after_create = {
            let mut cache = Cache::open_or_create(&cache_file_path).unwrap();
            cache.add_paths([PathBuf::from("yo")].into_iter()).unwrap();
            cache.last_update().unwrap()
        };
        std::thread::sleep(std::time::Duration::from_millis(10));
        let cache = Cache::open(&cache_file_path).unwrap();

        assert_eq!(last_update_after_create, cache.last_update().unwrap());
    }
}
