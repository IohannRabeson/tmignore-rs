use std::{
    collections::BTreeSet,
    ffi::OsStr,
    path::{Path, PathBuf},
    sync::Arc,
    thread::JoinHandle,
};

use crossbeam_channel::Receiver;

pub fn find_repositories(
    directories: &BTreeSet<PathBuf>,
    ignored_directories: &BTreeSet<PathBuf>,
) -> Option<(Receiver<PathBuf>, JoinHandle<()>)> {
    const DOT_GIT_DIRECTORY_NAME: &str = ".git";

    if directories.is_empty() {
        return None;
    }

    let ignored_directories = Arc::new(ignored_directories.clone());
    let walker = create_walk_builder(directories).build_parallel();
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

fn create_walk_builder(directories: &BTreeSet<PathBuf>) -> ignore::WalkBuilder {
    assert!(!directories.is_empty());
    let mut directories_iter = directories.iter();
    // Here it's guaranteed that directories contains something so we can unwrap
    let mut builder = ignore::WalkBuilder::new(directories_iter.next().unwrap());

    for directory in directories_iter {
        builder.add(directory);
    }

    builder.hidden(false);

    builder
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        path::{Path, PathBuf},
    };

    use temp_dir_builder::TempDirectoryBuilder;

    use crate::find_repositories::find_repositories;

    fn find_repositories_vec(
        directories: &[impl AsRef<Path>],
        ignored_directories: &BTreeSet<PathBuf>,
    ) -> Vec<PathBuf> {
        let mut results = vec![];
        let directories =
            BTreeSet::from_iter(directories.iter().map(AsRef::as_ref).map(Path::to_path_buf));
        match find_repositories(&directories, ignored_directories) {
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
}
