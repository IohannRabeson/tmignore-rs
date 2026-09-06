pub mod list;
pub mod monitor;
pub mod path;
pub mod reset;
pub mod run;
pub mod stats;

use std::{
    collections::{BTreeSet, HashSet},
    path::{Path, PathBuf},
};

use anyhow::anyhow;
use log::{error, info, warn};
use regex::RegexSet;

use crate::{
    diff::Exclusion,
    git,
    timemachine::{self, Error},
};

trait TimeMachineTrait {
    fn add_exclusions<'a>(paths: impl Iterator<Item = &'a PathBuf>) -> Vec<Error>;
    fn remove_exclusions<'a>(paths: impl Iterator<Item = &'a PathBuf>) -> Vec<Error>;
}

struct TimeMachine;

impl TimeMachineTrait for TimeMachine {
    fn add_exclusions<'a>(paths: impl Iterator<Item = &'a PathBuf>) -> Vec<Error> {
        timemachine::add_exclusions(paths)
    }

    fn remove_exclusions<'a>(paths: impl Iterator<Item = &'a PathBuf>) -> Vec<Error> {
        timemachine::remove_exclusions(paths)
    }
}

/// Calls `TM::add_exclusion` and `TM::remove_exclusion` depending on the diff.
/// Returns the list of paths failed to be added.
fn apply_diff_and_print<TM: TimeMachineTrait>(
    diff: &crate::diff::Diff,
    dry_run: bool,
    details: bool,
) -> HashSet<PathBuf> {
    let mut add_failed_paths = HashSet::new();

    // On a case insensitive filesystem a directory renamed by case only is removed under its old
    // spelling and added under the new one, and both spellings are the same item: excluding and
    // unexcluding the same item in one pass is not applied in the order the calls are made, so the
    // removal is dropped instead.
    let removed = removals_to_apply(diff);

    let mut remove_errors = Vec::new();
    if !dry_run {
        let mut exclusion_errors = TM::remove_exclusions(removed.iter().copied());

        remove_errors.append(&mut exclusion_errors);
    }

    let mut add_errors = Vec::new();
    if !dry_run {
        let mut exclusion_errors = TM::add_exclusions(diff.added.iter());
        for exclusion_error in &exclusion_errors {
            add_failed_paths.insert(exclusion_error.path.clone());
        }
        add_errors.append(&mut exclusion_errors);
    }

    let add_count = diff.added.len().saturating_sub(add_errors.len());
    let remove_count = removed.len();

    if add_count > 0 {
        info!(
            "Added {add_count} {} to the backup exclusion list",
            crate::text::plural("path", add_count)
        );
    }

    if remove_count > 0 {
        info!(
            "Removed {remove_count} {} from the backup exclusion list",
            crate::text::plural("path", remove_count)
        );
    }

    if add_count == 0 && remove_count == 0 {
        info!("No changes to the backup exclusion list");
    }

    if details {
        for path in &diff.added {
            if !add_failed_paths.contains(path) {
                info!("+ {}", path.display());
            }
        }
    }

    if details {
        for path in &removed {
            info!("- {}", path.display());
        }
    }

    for error in add_errors.iter().chain(remove_errors.iter()) {
        warn!("Error: {}: {}", error.path.display(), error.message);
    }

    add_failed_paths
}

fn removals_to_apply(diff: &crate::diff::Diff) -> Vec<&PathBuf> {
    if diff.added.is_empty() || diff.removed.is_empty() {
        return diff.removed.iter().collect();
    }

    let canonical_added: HashSet<PathBuf> = diff
        .added
        .iter()
        .filter_map(|path| path.canonicalize().ok())
        .collect();

    diff.removed
        .iter()
        .filter(|path| {
            !path
                .canonicalize()
                .is_ok_and(|canonical_path| canonical_added.contains(&canonical_path))
        })
        .collect()
}

fn create_whitelist(whitelist_patterns: &BTreeSet<String>) -> Result<RegexSet, regex::Error> {
    RegexSet::new(whitelist_patterns.iter().filter_map(|pattern| {
        match fnmatch_regex::glob_to_regex_pattern(pattern) {
            Ok(pattern) => Some(pattern),
            Err(error) => {
                error!("Error: invalid whitelist pattern '{pattern}': {error}");
                None
            }
        }
    }))
}

/// Find the paths in a repository to exclude from Time Machine backup.
/// If a path matches at least one of the regexes in the `whitelist` `RegexSet` it will not be
/// added to `exclusions`. Paths may be pushed more than once; callers that need uniqueness
/// must dedup after collecting.
fn find_paths_to_exclude_from_backup(
    repository_path: impl AsRef<Path>,
    whitelist: &RegexSet,
    exclusions: &mut Vec<Exclusion>,
) -> anyhow::Result<()> {
    #[cfg(test)]
    tests::FIND_PATHS_TO_EXCLUDE_CALLS.with(|calls| calls.set(calls.get() + 1));

    let repository_path = repository_path.as_ref();
    let ignored_files = git::find_ignored_files(repository_path)?;

    for ignored_file in ignored_files {
        if ignored_file
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            continue;
        }
        if let Some(ignored_file) = ignored_file.to_str()
            && whitelist.is_match(ignored_file)
        {
            continue;
        }
        exclusions.push(Exclusion::new(ignored_file, repository_path.to_path_buf()));
    }

    Ok(())
}

fn join_thread<T>(thread_handle: std::thread::JoinHandle<T>) -> anyhow::Result<T> {
    let thread_name = thread_handle
        .thread()
        .name()
        .unwrap_or("<unamed>")
        .to_string();

    match thread_handle.join() {
        Ok(result) => Ok(result),
        Err(error) => {
            if let Some(text) = error.downcast_ref::<&str>() {
                Err(anyhow!("Thread '{thread_name}' panicked: {text}"))
            } else if let Some(text) = error.downcast_ref::<String>() {
                Err(anyhow!("Thread '{thread_name}' panicked: {text}"))
            } else {
                Err(anyhow!("Thread '{thread_name}' panicked"))
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::{
        cell::RefCell,
        collections::BTreeSet,
        path::{Path, PathBuf},
        time::Duration,
    };

    use temp_dir_builder::{TempDirectory, TempDirectoryBuilder};

    use crate::{
        commands::{TimeMachineTrait, apply_diff_and_print, create_whitelist},
        config::Config,
        diff::Diff,
        timemachine::Error,
    };

    thread_local! {
        pub(crate) static FIND_PATHS_TO_EXCLUDE_CALLS: std::cell::Cell<usize> =
            const { std::cell::Cell::new(0) };
    }

    /// Return the path of a test directory in the crate directory, deleted first if a previous
    /// run left it behind.
    ///
    /// `std::env::temp_dir` is unusable: it is excluded from Time Machine.
    pub(crate) fn prepare_test_directory(name: &str) -> PathBuf {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(name);

        if path.is_dir() {
            std::fs::remove_dir_all(&path).unwrap();
        }

        path
    }

    /// Create a Git repository with some files.
    ///
    /// When `root_directory` is None, the temporary directory is created in `/tmp`
    /// which is excluded from Time Machine backup, meaning all children files and directories
    /// will be considered excluded from Time Machine backup anyway (`tmutil isexcluded` will always returns "[Excluded]").
    pub(crate) fn create_repository(root_directory: Option<&Path>) -> TempDirectory {
        if let Some(root_directory) = root_directory
            && root_directory.exists()
            && root_directory.is_dir()
        {
            std::fs::remove_dir_all(root_directory).unwrap();
        }
        let mut temp_dir_builder = TempDirectoryBuilder::default();
        if let Some(root_directory) = root_directory {
            temp_dir_builder = temp_dir_builder.root_folder(root_directory);
        }
        let temp_dir = temp_dir_builder
            .add_text_file(".gitignore", "a\nb\n")
            .add_empty_file("a")
            .add_empty_file("b")
            .add_empty_file("c")
            .build()
            .unwrap();

        init_git_repository(temp_dir.path());

        temp_dir
    }

    pub(crate) fn init_git_repository(directory_path: impl AsRef<Path>) {
        std::process::Command::new("git")
            .arg("init")
            .arg(directory_path.as_ref())
            .output()
            .unwrap();
    }

    pub(crate) fn run_git(args: &[&str]) {
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

    pub(crate) fn create_config(search_directory: impl AsRef<Path>) -> Config {
        let mut config = Config {
            debounce_duration: Duration::from_secs(1),
            ..Default::default()
        };
        config.search_directories.clear();
        config
            .search_directories
            .insert(search_directory.as_ref().to_path_buf());

        config
    }

    pub(crate) static SIGINT_LOG: std::sync::Mutex<Vec<(std::time::Instant, String)>> =
        std::sync::Mutex::new(Vec::new());

    pub(crate) fn send_sigint() {
        if let Ok(mut log) = SIGINT_LOG.lock() {
            log.push((
                std::time::Instant::now(),
                std::thread::current()
                    .name()
                    .unwrap_or("<unnamed>")
                    .to_string(),
            ));
        }

        unsafe {
            libc::kill(libc::getpid(), signal_hook::consts::SIGINT);
        }
    }

    pub(crate) fn sigint_log_report(reference: std::time::Instant) -> String {
        let Ok(log) = SIGINT_LOG.lock() else {
            return String::from("<poisoned>");
        };

        let entries: Vec<String> = log
            .iter()
            .map(|(instant, thread)| {
                if *instant >= reference {
                    format!(
                        "{thread} +{}ms",
                        instant.duration_since(reference).as_millis()
                    )
                } else {
                    format!(
                        "{thread} -{}ms",
                        reference.duration_since(*instant).as_millis()
                    )
                }
            })
            .collect();

        format!("{} sigints: [{}]", entries.len(), entries.join(", "))
    }

    #[test]
    fn test_create_whitelist_invalid() {
        let patterns = BTreeSet::from([String::from("[z-a].txt")]);
        let result = create_whitelist(&patterns).unwrap();
        assert!(result.is_empty());
    }

    struct MockTimeMachineError;

    impl TimeMachineTrait for MockTimeMachineError {
        fn add_exclusions<'a>(paths: impl Iterator<Item = &'a PathBuf>) -> Vec<Error> {
            paths
                .map(|path| Error {
                    path: path.clone(),
                    message: "fail".into(),
                })
                .collect()
        }

        fn remove_exclusions<'a>(paths: impl Iterator<Item = &'a PathBuf>) -> Vec<Error> {
            paths
                .map(|path| Error {
                    path: path.clone(),
                    message: "fail".into(),
                })
                .collect()
        }
    }

    #[test]
    fn test_apply_diff_add_print_error() {
        let temp_dir = TempDirectoryBuilder::default()
            .add_empty_file("a")
            .build()
            .unwrap();
        let diff = Diff {
            added: BTreeSet::from([temp_dir.path().join("a")]),
            removed: BTreeSet::new(),
        };
        let error_paths = apply_diff_and_print::<MockTimeMachineError>(&diff, false, false);

        assert_eq!(1, error_paths.len());
    }

    #[test]
    fn test_apply_diff_remove_print_error() {
        let temp_dir = TempDirectoryBuilder::default()
            .add_empty_file("a")
            .build()
            .unwrap();
        let diff = Diff {
            removed: BTreeSet::from([temp_dir.path().join("a")]),
            added: BTreeSet::new(),
        };
        let _ = apply_diff_and_print::<MockTimeMachineError>(&diff, false, false);
    }

    thread_local! {
        static RECORDED_CALLS: RefCell<Vec<(&'static str, PathBuf)>> = const { RefCell::new(Vec::new()) };
    }

    struct MockTimeMachineRecorder;

    impl TimeMachineTrait for MockTimeMachineRecorder {
        fn add_exclusions<'a>(paths: impl Iterator<Item = &'a PathBuf>) -> Vec<Error> {
            RECORDED_CALLS.with_borrow_mut(|calls| {
                calls.extend(paths.map(|path| ("add", path.clone())));
            });

            Vec::new()
        }

        fn remove_exclusions<'a>(paths: impl Iterator<Item = &'a PathBuf>) -> Vec<Error> {
            RECORDED_CALLS.with_borrow_mut(|calls| {
                calls.extend(paths.map(|path| ("remove", path.clone())));
            });

            Vec::new()
        }
    }

    #[test]
    fn test_apply_diff_does_not_remove_an_item_it_adds_under_another_spelling() {
        let temp_dir = TempDirectoryBuilder::default()
            .add_empty_file("foo/a")
            .build()
            .unwrap();
        let lower_case_path = temp_dir.path().join("foo");
        let upper_case_path = temp_dir.path().join("Foo");

        assert!(
            upper_case_path.exists(),
            "this test needs a case insensitive filesystem"
        );

        let diff = Diff {
            added: BTreeSet::from([lower_case_path.clone()]),
            removed: BTreeSet::from([upper_case_path]),
        };

        RECORDED_CALLS.with_borrow_mut(Vec::clear);

        let error_paths = apply_diff_and_print::<MockTimeMachineRecorder>(&diff, false, false);

        assert!(error_paths.is_empty());
        assert_eq!(
            vec![("add", lower_case_path)],
            RECORDED_CALLS.with_borrow(Clone::clone),
            "both spellings are the same item, so removing the old one would undo the addition of \
             the new one"
        );
    }
}
