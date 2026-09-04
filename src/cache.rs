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

use crate::diff::{Diff, Exclusion};

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
    M::up(include_str!("sql/v2.sql")),
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

    const SQL_INSERT_PATH: &str = "INSERT INTO paths (path, repository) VALUES (?, ?)";
    const SQL_SET_LAST_UPDATE: &str = "UPDATE metadata SET last_update=?";

    pub fn reset(&mut self, iter: impl IntoIterator<Item = Exclusion>) -> anyhow::Result<()> {
        let mut connection = self.connection.borrow_mut();
        let mut transaction = connection.transaction()?;
        let mut insert_stmt = transaction.prepare(Self::SQL_INSERT_PATH)?;
        transaction.execute("DELETE FROM paths", params![])?;
        for exclusion in iter {
            insert_stmt.execute(params![
                path_to_bytes(exclusion.path()),
                exclusion.repository().map(path_to_bytes)
            ])?;
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

    pub fn add_paths(&mut self, iter: impl Iterator<Item = Exclusion>) -> anyhow::Result<()> {
        let mut connection = self.connection.borrow_mut();
        let mut transaction = connection.transaction()?;
        let mut insert_stmt = transaction.prepare(Self::SQL_INSERT_PATH)?;
        for exclusion in iter {
            insert_stmt.execute(params![
                path_to_bytes(exclusion.path()),
                exclusion.repository().map(path_to_bytes)
            ])?;
        }
        drop(insert_stmt);
        Self::set_last_update_transaction(&mut transaction)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn remove_paths<'a>(
        &mut self,
        paths: impl Iterator<Item = &'a PathBuf>,
        repository: &Path,
    ) -> anyhow::Result<()> {
        let repository = path_to_bytes(repository);
        let mut connection = self.connection.borrow_mut();
        let mut transaction = connection.transaction()?;
        {
            let mut delete_stmt =
                transaction.prepare("DELETE FROM paths WHERE path = ? AND repository = ?")?;
            for path in paths {
                delete_stmt.execute(params![path_to_bytes(path), repository])?;
            }
        }
        Self::set_last_update_transaction(&mut transaction)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn paths_created_by(&self, repository: impl AsRef<Path>) -> anyhow::Result<Vec<PathBuf>> {
        let connection = self.connection.borrow();
        let mut select_stmt = connection.prepare("SELECT path FROM paths WHERE repository = ?")?;
        let paths = select_stmt.query_map(params![path_to_bytes(repository.as_ref())], |row| {
            let bytes: Vec<u8> = row.get(0)?;

            Ok(PathBuf::from(OsStr::from_bytes(&bytes)))
        })?;

        Ok(paths.filter_map(Result::ok).collect())
    }

    /// `exclusions` must already be sorted by path: this uses `binary_search` against it.
    pub fn find_diff(&self, exclusions: &[Exclusion]) -> anyhow::Result<Diff> {
        let connection = self.connection.borrow();
        let mut select_stmt = connection.prepare("SELECT DISTINCT path FROM paths")?;
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
        let mut stmt = connection.prepare("SELECT * FROM paths WHERE path = ?")?;
        let mut current = path.as_ref().parent();

        while let Some(ancestor) = current {
            if stmt.exists(params![path_to_bytes(ancestor)])? {
                return Ok(true);
            }
            current = ancestor.parent();
        }

        Ok(false)
    }

    pub fn paths(&self) -> anyhow::Result<Vec<PathBuf>> {
        let connection = self.connection.borrow();
        let mut stmt = connection.prepare("SELECT DISTINCT path FROM paths")?;
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
pub(crate) mod tests {
    use std::{
        assert_matches,
        collections::BTreeSet,
        path::{Path, PathBuf},
    };

    use rstest::rstest;
    use temp_dir_builder::TempDirectoryBuilder;

    use crate::{
        cache::{MIGRATIONS_SLICE, OpenOrCreateError},
        diff::Exclusion,
    };

    use super::Cache;

    pub(crate) fn write_version_1_cache(cache_file_path: &Path, paths: &[PathBuf]) {
        let connection = rusqlite::Connection::open(cache_file_path).unwrap();
        connection
            .execute_batch(include_str!("sql/v0.sql"))
            .unwrap();
        connection
            .execute_batch(include_str!("sql/v1.sql"))
            .unwrap();
        connection.pragma_update(None, "user_version", 2).unwrap();
        let mut insert_stmt = connection
            .prepare("INSERT INTO paths (path) VALUES (?)")
            .unwrap();
        for path in paths {
            insert_stmt
                .execute(super::params![super::path_to_bytes(path)])
                .unwrap();
        }
    }

    fn orphans<const N: usize>(paths: [&str; N]) -> [Exclusion; N] {
        paths.map(|path| Exclusion::orphan(PathBuf::from(path)))
    }

    fn owned<const N: usize>(repository: &str, paths: [&str; N]) -> [Exclusion; N] {
        paths.map(|path| Exclusion::new(PathBuf::from(path), PathBuf::from(repository)))
    }

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

        cache.reset(orphans(["hello"])).unwrap();
        let first_update = cache.last_update().unwrap().unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));
        cache.reset(orphans(["world"])).unwrap();
        let second_update = cache.last_update().unwrap().unwrap();

        assert!(second_update > first_update);
    }

    #[test]
    fn test_add_paths_sets_last_update() {
        let mut cache = Cache::open_in_memory().unwrap();
        assert!(cache.last_update().unwrap().is_none());

        cache.add_paths(orphans(["hello"]).into_iter()).unwrap();
        let first_update = cache.last_update().unwrap().unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));
        cache.add_paths(orphans(["world"]).into_iter()).unwrap();
        let second_update = cache.last_update().unwrap().unwrap();

        assert!(second_update > first_update);
    }

    #[test]
    fn test_remove_paths_sets_last_update() {
        let mut cache = Cache::open_in_memory().unwrap();
        assert!(cache.last_update().unwrap().is_none());

        cache
            .remove_paths([PathBuf::from("hello")].iter(), Path::new("/repo"))
            .unwrap();
        let first_update = cache.last_update().unwrap().unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));
        cache
            .remove_paths([PathBuf::from("world")].iter(), Path::new("/repo"))
            .unwrap();
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
        cache.reset(orphans(["hello", "world"])).unwrap();
        assert_eq!(2, cache.paths().unwrap().len());
    }

    #[test]
    fn test_find_diff() {
        let mut cache = Cache::open_in_memory().unwrap();
        cache.reset(orphans(["hello", "world"])).unwrap();
        let mut exclusions = owned("/repo", ["world", "hey"]);
        exclusions.sort_unstable();
        let diff = cache.find_diff(&exclusions).unwrap();
        assert_eq!(1, diff.added.len());
        assert!(diff.added.contains(&PathBuf::from("hey")));
        assert_eq!(1, diff.removed.len());
        assert!(diff.removed.contains(&PathBuf::from("hello")));
    }

    #[test]
    fn test_paths_created_by() {
        let mut cache = Cache::open_in_memory().unwrap();
        let mut exclusions = owned("/repo", ["/repo/a", "/repo/b"]).to_vec();
        exclusions.extend(owned("/repo/nested", ["/repo/nested/c"]));
        exclusions.extend(owned("/repo-sibling", ["/repo-sibling/d"]));
        exclusions.extend(orphans(["/repo/e"]));
        cache.reset(exclusions).unwrap();

        let paths: BTreeSet<_> = cache
            .paths_created_by("/repo")
            .unwrap()
            .into_iter()
            .collect();

        assert_eq!(
            BTreeSet::from([PathBuf::from("/repo/a"), PathBuf::from("/repo/b")]),
            paths,
            "only the exclusions '/repo' created must be reported: not those of the repository \
             nested in it, not those of a repository whose path shares its prefix, and not those \
             read from a cache written before the repository was recorded"
        );
    }

    #[test]
    fn test_paths_created_by_reports_a_path_two_repositories_created() {
        let mut cache = Cache::open_in_memory().unwrap();
        let mut exclusions = owned("/repo", ["/repo/shared"]).to_vec();
        exclusions.extend(owned("/other", ["/repo/shared"]));
        cache.reset(exclusions).unwrap();

        assert_eq!(
            vec![PathBuf::from("/repo/shared")],
            cache.paths_created_by("/repo").unwrap()
        );
        assert_eq!(
            vec![PathBuf::from("/repo/shared")],
            cache.paths_created_by("/other").unwrap()
        );
        assert_eq!(
            vec![PathBuf::from("/repo/shared")],
            cache.paths().unwrap(),
            "a path two repositories created is one exclusion"
        );
    }

    #[test]
    fn test_remove_paths() {
        let mut cache = Cache::open_in_memory().unwrap();

        cache
            .reset(owned("/repo", ["/repo/removed", "/repo/kept"]))
            .unwrap();
        cache
            .remove_paths([PathBuf::from("/repo/removed")].iter(), Path::new("/repo"))
            .unwrap();

        assert_eq!(
            vec![PathBuf::from("/repo/kept")],
            cache.paths().unwrap(),
            "the path that was not listed was deleted"
        );
    }

    #[test]
    fn test_remove_paths_only_removes_what_the_repository_created() {
        let mut cache = Cache::open_in_memory().unwrap();

        let mut exclusions = owned("/repo", ["/repo/shared"]).to_vec();
        exclusions.extend(owned("/other", ["/repo/shared"]));
        cache.reset(exclusions).unwrap();
        cache
            .remove_paths([PathBuf::from("/repo/shared")].iter(), Path::new("/repo"))
            .unwrap();

        assert!(
            cache.paths_created_by("/repo").unwrap().is_empty(),
            "the exclusion '/repo' created was not deleted"
        );
        assert_eq!(
            vec![PathBuf::from("/repo/shared")],
            cache.paths_created_by("/other").unwrap(),
            "removing the exclusion '/repo' created also deleted the one '/other' created for \
             the same path"
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
            .reset(owned("/repo", ["/repo/target", "/repo/a"]))
            .unwrap();

        assert_eq!(expected, cache.contains_ancestor_of(path).unwrap());
    }

    #[test]
    fn test_open_migrates_a_cache_written_before_the_repository_was_recorded() {
        let temp_dir = TempDirectoryBuilder::default().build().unwrap();
        let cache_file_path = temp_dir.path().join("cache.db");
        write_version_1_cache(
            &cache_file_path,
            &[PathBuf::from("/repo/big_dir/"), PathBuf::from("/repo/a")],
        );

        let cache = Cache::open(&cache_file_path).unwrap();

        assert_eq!(
            u32::try_from(MIGRATIONS_SLICE.len()),
            Ok(cache.get_version().unwrap())
        );
        assert_eq!(
            2,
            cache.paths().unwrap().len(),
            "the exclusions of the previous schema must survive the migration"
        );
        assert!(
            cache.paths_created_by("/repo").unwrap().is_empty(),
            "no repository created these exclusions, so no rescan of a repository may remove them"
        );
        assert_eq!(
            2,
            cache.find_diff(&[]).unwrap().removed.len(),
            "a full scan must still see them, otherwise an exclusion that is no longer gitignored \
             would stay in the Time Machine exclusion list forever"
        );
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
            cache.add_paths(orphans(["yo"]).into_iter()).unwrap();
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
            cache.add_paths(orphans(["yo"]).into_iter()).unwrap();
            cache.last_update().unwrap()
        };
        std::thread::sleep(std::time::Duration::from_millis(10));
        let cache = Cache::open(&cache_file_path).unwrap();

        assert_eq!(last_update_after_create, cache.last_update().unwrap());
    }
}
