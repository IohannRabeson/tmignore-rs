use std::{
    collections::BTreeSet,
    ffi::OsStr,
    path::{Path, PathBuf},
    sync::Arc,
    thread::JoinHandle,
};

use crossbeam_channel::Receiver;

const DOT_GIT_DIRECTORY_NAME: &str = ".git";

pub fn find_repositories(
    directories: &BTreeSet<PathBuf>,
    ignored_directories: &BTreeSet<PathBuf>,
    threads: usize,
) -> Option<(Receiver<PathBuf>, JoinHandle<()>)> {
    if directories.is_empty() {
        return None;
    }

    let ignored_directories = Arc::new(ignored_directories.clone());
    let walker = create_walk_builder(directories, true)
        .threads(threads)
        .build_parallel();
    let (tx, rx) = crossbeam_channel::bounded(128);
    let thread_handle = std::thread::spawn(move || {
        walker.run(|| {
            let tx = tx.clone();
            let ignored_directories = ignored_directories.clone();

            Box::new(move |entry| {
                use ignore::WalkState;
                if let Ok(entry) = entry
                    && entry.path().is_dir()
                {
                    if ignored_directories.contains(entry.path()) {
                        return WalkState::Skip;
                    }
                    if entry.file_name() == OsStr::new(DOT_GIT_DIRECTORY_NAME)
                        && let Some(parent) = entry.path().parent()
                    {
                        tx.send(parent.to_path_buf()).unwrap();
                    }
                }

                WalkState::Continue
            })
        });
    });

    Some((rx, thread_handle))
}

fn create_walk_builder(directories: &BTreeSet<PathBuf>, ignore: bool) -> ignore::WalkBuilder {
    assert!(!directories.is_empty());
    let mut directories_iter = directories.iter();
    // Here it's guaranteed that directories contains something so we can unwrap
    let mut builder = ignore::WalkBuilder::new(directories_iter.next().unwrap());

    for directory in directories_iter {
        builder.add(directory);
    }

    builder
        .ignore(ignore)
        .git_exclude(ignore)
        .git_global(ignore)
        .git_ignore(ignore)
        .hidden(false);

    builder
}

#[derive(thiserror::Error, Debug)]
pub enum FindIgnoredFileError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("Git command failed: {0}")]
    CommandFailed(String),
}

pub fn find_ignored_files(
    repository_directory: &Path,
) -> Result<Vec<PathBuf>, FindIgnoredFileError> {
    if !repository_directory.exists() {
        return Ok(vec![])
    }

    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repository_directory)
        .arg("ls-files")
        .arg("--directory")
        .arg("--exclude-standard")
        .arg("--ignored")
        .arg("--others")
        .arg("-z")
        .output()?;

    if !output.status.success() {
        return Err(FindIgnoredFileError::CommandFailed(
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }

    Ok(output
        .stdout
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .filter_map(|bytes| {
            std::str::from_utf8(bytes)
                .ok()
                .map(|s| repository_directory.join(s))
        })
        .collect())
}

pub fn find_parent_repository(path: impl AsRef<Path>) -> Option<PathBuf> {
    let mut path = path.as_ref();

    loop {
        if path.join(DOT_GIT_DIRECTORY_NAME).is_dir() {
            return Some(path.to_path_buf());
        }

        path = path.parent()?;
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        path::{Path, PathBuf},
    };

    use temp_dir_builder::TempDirectoryBuilder;

    use crate::git::{find_parent_repository, find_repositories};

    fn find_repositories_vec(
        directories: &[impl AsRef<Path>],
        ignored_directories: &BTreeSet<PathBuf>,
    ) -> Vec<PathBuf> {
        let mut results = vec![];
        let directories =
            BTreeSet::from_iter(directories.iter().map(AsRef::as_ref).map(Path::to_path_buf));
        match find_repositories(&directories, ignored_directories, 0) {
            Some((rx, thread_handle)) => {
                while let Ok(path) = rx.recv() {
                    results.push(path);
                }
                thread_handle.join().unwrap();
            }
            None => (),
        }

        results
    }

    #[test]
    fn test_root_directory() {
        let temp_dir = TempDirectoryBuilder::default()
            .add_directory(".git")
            .build()
            .unwrap();
        let ignored_directories = BTreeSet::new();
        let repositories = find_repositories_vec(&[temp_dir.path()], &ignored_directories);

        assert_eq!(repositories.len(), 1);
        assert_eq!(repositories[0], temp_dir.path());
    }

    #[test]
    fn test_sub_directory() {
        let temp_dir = TempDirectoryBuilder::default()
            .add_directory("subdirectory/.git")
            .build()
            .unwrap();
        let ignored_directories = BTreeSet::new();
        let repositories = find_repositories_vec(&[temp_dir.path()], &ignored_directories);

        assert_eq!(repositories.len(), 1);
        assert_eq!(repositories[0], temp_dir.path().join("subdirectory"));
    }

    #[test]
    fn test_nested_repositories() {
        let temp_dir = TempDirectoryBuilder::default()
            .add_directory("subdirectory/.git")
            .add_directory("subdirectory/sub_subdirectory/.git")
            .build()
            .unwrap();
        let ignored_directories = BTreeSet::new();
        let repositories = find_repositories_vec(&[temp_dir.path()], &ignored_directories);

        assert_eq!(repositories.len(), 2);
        assert!(
            repositories.contains(
                &temp_dir
                    .path()
                    .join("subdirectory")
                    .join("sub_subdirectory")
            )
        );
        assert!(repositories.contains(&temp_dir.path().join("subdirectory")));
    }

    #[test]
    fn test_ignored_repositories() {
        let temp_dir = TempDirectoryBuilder::default()
            .add_directory("subdirectory/.git")
            .add_directory("subdirectory/ignored/.git")
            .build()
            .unwrap();
        let ignored_directories = BTreeSet::from([temp_dir
            .path()
            .join("subdirectory")
            .join("ignored")
            .to_path_buf()]);
        let repositories = find_repositories_vec(&[temp_dir.path()], &ignored_directories);

        assert_eq!(repositories.len(), 1);
        assert_eq!(repositories[0], temp_dir.path().join("subdirectory"));
    }

    #[test]
    fn test_find_parent_directory_success() {
        let temp_dir = TempDirectoryBuilder::default()
            .add_directory("repository/suddir/subsub")
            .add_directory("repository/.git")
            .add_directory("not_a_repo/sub")
            .build()
            .unwrap();
        let repository_path = temp_dir.path().join("repository");
        let subdir_path = repository_path.join("suddir");
        let subsub_path = subdir_path.join("suddir");
        let not_a_repo_sub = temp_dir.path().join("not_a_repo").join("sub");

        assert_eq!(
            find_parent_repository(&subdir_path).as_ref(),
            Some(&repository_path)
        );
        assert_eq!(
            find_parent_repository(&subsub_path).as_ref(),
            Some(&repository_path)
        );
        assert_eq!(find_parent_repository(&not_a_repo_sub), None);
    }
}
