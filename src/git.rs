use std::{
    collections::BTreeSet,
    ffi::OsStr,
    path::{Path, PathBuf},
    sync::Arc,
    thread::JoinHandle,
};

use crossbeam_channel::Receiver;
use log::warn;

const DOT_GIT_DIRECTORY_NAME: &str = ".git";

fn git_command() -> std::process::Command {
    // Disable core.fsmonitor: a malicious repository could otherwise set it in
    // its local config to run an arbitrary command when we scan it. A command
    // line '-c' overrides the repository configuration.
    let mut command = std::process::Command::new("/usr/bin/git");
    command.arg("-c").arg("core.fsmonitor=");
    command
}

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
                if let Ok(entry) = entry {
                    // A git worktree contains a file named '.git' instead of a directory
                    // and they must be treated just like a regular repository with its
                    // own .gitignore that can be different from the .gitignore of the parent
                    // repository.
                    if entry.path().is_file() {
                        if entry.file_name() == OsStr::new(DOT_GIT_DIRECTORY_NAME)
                            && let Some(parent) = entry.path().parent()
                        {
                            let _ = tx.send(parent.to_path_buf());
                        }
                    } else if entry.path().is_dir() {
                        if ignored_directories.contains(entry.path()) {
                            return WalkState::Skip;
                        }
                        if entry.file_name() == OsStr::new(DOT_GIT_DIRECTORY_NAME)
                            && let Some(parent) = entry.path().parent()
                        {
                            let _ = tx.send(parent.to_path_buf());
                        }
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

    // This unwrap will be useless when https://github.com/BurntSushi/ripgrep/pull/3261 will be merged.
    #[allow(clippy::unwrap_used, reason = "directories is guaranteed non-empty")]
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

pub fn find_ignored_files(repository_directory: &Path) -> anyhow::Result<Vec<PathBuf>> {
    if !repository_directory.exists() {
        return Ok(vec![]);
    }

    let repository_directory = repository_directory.canonicalize()?;

    let output = git_command()
        .arg("-C")
        .arg(&repository_directory)
        .arg("ls-files")
        .arg("--directory")
        .arg("--exclude-standard")
        .arg("--ignored")
        .arg("--others")
        .arg("-z")
        .output()?;

    if !output.status.success() {
        warn!(
            "Failed to find ignored file in repository '{}': {}",
            repository_directory.display(),
            String::from_utf8_lossy(&output.stderr)
        );

        return Ok(vec![]);
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
        if path.join(DOT_GIT_DIRECTORY_NAME).exists() {
            return Some(path.to_path_buf());
        }

        path = path.parent()?;
    }
}

/// Execute git config --global --get core.excludesFile
pub fn get_global_git_ignore() -> Option<PathBuf> {
    get_global_git_ignore_from(std::env::current_dir().ok()?)
}

fn get_global_git_ignore_from(working_directory: impl AsRef<Path>) -> Option<PathBuf> {
    let output = git_command()
        .current_dir(working_directory)
        .arg("config")
        .arg("--global")
        .arg("--get")
        .arg("core.excludesFile")
        .output()
        .ok()?;

    let stdout = String::from_utf8(output.stdout).ok()?;
    let stdout = stdout.trim();

    if stdout.is_empty() {
        return None;
    }

    let global_gitignore_path = PathBuf::from(stdout);

    if !global_gitignore_path.is_file() {
        return None;
    }

    Some(global_gitignore_path)
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

    fn run_git(args: &[&str]) {
        let output = std::process::Command::new("/usr/bin/git")
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn test_get_global_git_ignore_ignores_repository_local_config() {
        let temp_dir = TempDirectoryBuilder::default()
            .add_text_file("repository/local_excludes", "secrets\n")
            .build()
            .unwrap();
        let repository_path = temp_dir.path().join("repository");
        let excludes_path = repository_path.join("local_excludes");
        run_git(&["init", "-q", repository_path.to_str().unwrap()]);
        run_git(&[
            "-C",
            repository_path.to_str().unwrap(),
            "config",
            "core.excludesFile",
            excludes_path.to_str().unwrap(),
        ]);

        let result = super::get_global_git_ignore_from(&repository_path);

        assert_ne!(
            Some(excludes_path),
            result,
            "get_global_git_ignore returned the repository-local core.excludesFile"
        );
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
    fn test_find_parent_repository_success() {
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

    #[test]
    fn test_find_parent_repository_worktree() {
        let temp_dir = TempDirectoryBuilder::default()
            .add_directory("worktree/subdir")
            .add_empty_file("worktree/.git")
            .build()
            .unwrap();
        let worktree_path = temp_dir.path().join("worktree");
        let subdir_path = worktree_path.join("subdir");

        assert_eq!(
            find_parent_repository(&subdir_path).as_ref(),
            Some(&worktree_path)
        );
    }

    #[test]
    fn test_find_ignored_files_not_a_git_repository() {
        let temp_dir = TempDirectoryBuilder::default().build().unwrap();

        assert!(
            super::find_ignored_files(temp_dir.path())
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn test_find_ignored_files_path_does_not_exist() {
        assert!(
            super::find_ignored_files(Path::new("/this/path/does/not/exist"))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn test_worktree() {
        let temp_dir = TempDirectoryBuilder::default()
            .add_directory("worktree")
            .add_empty_file("worktree/.git")
            .build()
            .unwrap();
        let ignored_directories = BTreeSet::new();
        let repositories = find_repositories_vec(&[temp_dir.path()], &ignored_directories);

        assert_eq!(repositories.len(), 1);
        assert_eq!(repositories[0], temp_dir.path().join("worktree"));
    }

    #[test]
    fn test_find_ignored_files_does_not_execute_repo_config_hooks() {
        use std::os::unix::fs::PermissionsExt;

        let temp_dir = TempDirectoryBuilder::default()
            .add_text_file(".gitignore", "ignored/\n")
            .add_empty_file("ignored/file")
            .build()
            .unwrap();
        let repository_path = temp_dir.path().to_path_buf();
        crate::commands::tests::init_git_repository(&repository_path);

        // A repository can configure git to run an arbitrary command (here via
        // core.fsmonitor) in its local .git/config. Running git plumbing inside
        // an untrusted repository must not execute it.
        let marker = temp_dir.path().join("executed_marker");
        let hook = temp_dir.path().join("hook.sh");
        // A script that creates the marker file when executed.
        std::fs::write(&hook, format!("#!/bin/sh\necho executed > {marker:?}\n")).unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        run_git(&[
            "-C",
            repository_path.to_str().unwrap(),
            "config",
            "core.fsmonitor",
            hook.to_str().unwrap(),
        ]);

        assert!(!marker.exists());

        let _ = super::find_ignored_files(&repository_path).unwrap();

        assert!(
            !marker.exists(),
            "find_ignored_files executed a command from the repository's local git config"
        );
    }

    #[test]
    fn test_find_ignored_files_does_not_execute_worktree_config_hooks() {
        use std::os::unix::fs::PermissionsExt;

        let temp_dir = TempDirectoryBuilder::default().build().unwrap();
        let main_repo = temp_dir.path().join("main");
        let worktree = temp_dir.path().join("worktree");
        let main_str = main_repo.to_str().unwrap();
        let worktree_str = worktree.to_str().unwrap();

        // A linked worktree's '.git' is a file pointing at the main repository's
        // git directory, so the main repository's config (here core.fsmonitor)
        // applies when we scan the worktree. It must not be executed either.
        let marker = temp_dir.path().join("executed_marker");
        let hook = temp_dir.path().join("hook.sh");
        std::fs::write(&hook, format!("#!/bin/sh\necho executed > {marker:?}\n")).unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();

        run_git(&["init", "-q", main_str]);
        run_git(&[
            "-C",
            main_str,
            "-c",
            "user.email=a@b.c",
            "-c",
            "user.name=a",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ]);
        run_git(&["-C", main_str, "worktree", "add", "-q", worktree_str]);
        std::fs::write(worktree.join(".gitignore"), "ignored/\n").unwrap();
        std::fs::create_dir(worktree.join("ignored")).unwrap();
        std::fs::write(worktree.join("ignored").join("a"), "x").unwrap();
        // Configure core.fsmonitor last: 'worktree add' refreshes the index and
        // would trigger the hook itself, so the marker must only be able to
        // appear because of find_ignored_files below.
        run_git(&[
            "-C",
            main_str,
            "config",
            "core.fsmonitor",
            hook.to_str().unwrap(),
        ]);

        assert!(!marker.exists());

        let _ = super::find_ignored_files(&worktree).unwrap();

        assert!(
            !marker.exists(),
            "find_ignored_files executed a command from the worktree's git config"
        );
    }
}
