use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

pub fn find_repositories(directories: &[impl AsRef<Path>]) -> Vec<PathBuf> {
    const DOT_GIT_DIRECTORY_NAME: &str = ".git";

    if directories.is_empty() {
        return vec![];
    }

    let mut results = vec![];
    let walker = create_walk_builder(directories).build_parallel();
    let (tx, rx) = crossbeam_channel::bounded(128);

    let thread_handle = std::thread::spawn(move || {
        walker.run(|| {
            let tx = tx.clone();
            Box::new(move |entry| {
                use ignore::WalkState;

                if let Ok(entry) = entry
                    && entry.path().is_dir()
                    && entry.file_name() == OsStr::new(DOT_GIT_DIRECTORY_NAME)
                    && let Some(parent) = entry.path().parent()
                {
                    tx.send(parent.to_path_buf()).unwrap();
                }

                WalkState::Continue
            })
        });
    });

    while let Ok(path) = rx.recv() {
        results.push(path);
    }

    thread_handle.join().unwrap();

    results
}

fn create_walk_builder(directories: &[impl AsRef<Path>]) -> ignore::WalkBuilder {
    let mut builder = ignore::WalkBuilder::new(&directories[0]);

    for directory in directories.iter().skip(1) {
        builder.add(directory);
    }

    builder.hidden(false);

    builder
}

#[cfg(test)]
mod tests {
    use temp_dir_builder::TempDirectoryBuilder;

    use crate::find_repositories::find_repositories;

    #[test]
    fn test_root_directory() {
        let temp_dir = TempDirectoryBuilder::default()
            .add_directory(".git")
            .build()
            .unwrap();

        let repositories = find_repositories(&[temp_dir.path()]);

        assert_eq!(repositories.len(), 1);
        assert_eq!(repositories[0], temp_dir.path());
    }

    #[test]
    fn test_sub_directory() {
        let temp_dir = TempDirectoryBuilder::default()
            .add_directory("subdirectory/.git")
            .build()
            .unwrap();

        let repositories = find_repositories(&[temp_dir.path()]);

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

        let repositories = find_repositories(&[temp_dir.path()]);

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
}
