pub mod list;
pub mod monitor;
pub mod reset;
pub mod run;

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use log::{error, warn};
use regex::RegexSet;

use crate::{
    Logger, git,
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
    logger: &mut Logger,
) -> Vec<PathBuf> {
    let mut add_failed_paths = BTreeSet::new();

    let mut add_errors = Vec::new();
    if !dry_run {
        let mut exclusion_errors = TM::add_exclusions(diff.added.iter());
        for exclusion_error in &exclusion_errors {
            add_failed_paths.insert(exclusion_error.path.clone());
        }
        add_errors.append(&mut exclusion_errors);
    }

    let mut remove_errors = Vec::new();
    if !dry_run {
        let mut exclusion_errors = TM::remove_exclusions(diff.removed.iter());

        remove_errors.append(&mut exclusion_errors);
    }

    let add_count = diff.added.len() - add_errors.len();
    let remove_count = diff.removed.len();

    if add_count > 0 {
        logger.log(format!(
            "Added {add_count} paths to the backup exclusion list"
        ));
    }

    if remove_count > 0 {
        logger.log(format!(
            "Removed {remove_count} paths from the backup exclusion list"
        ));
    }

    if add_count == 0 && remove_count == 0 {
        logger.log("No changes to the backup exclusion list");
    }

    if details {
        for path in &diff.added {
            if !add_failed_paths.contains(path) {
                logger.log(format!("+ {}", path.display()));
            }
        }
    }

    if details {
        for path in &diff.removed {
            logger.log(format!("- {}", path.display()));
        }
    }

    for error in add_errors.iter().chain(remove_errors.iter()) {
        warn!("Error: {}: {}", error.path.display(), error.message);
    }

    add_errors.into_iter().map(|error| error.path).collect()
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
/// added to the `exclusion` set.
fn find_paths_to_exclude_from_backup(
    repository_path: impl AsRef<Path>,
    whitelist: &RegexSet,
    exclusions: &mut BTreeSet<std::path::PathBuf>,
) -> anyhow::Result<()> {
    let repository_path = repository_path.as_ref();
    let ignored_files = git::find_ignored_files(repository_path)?;

    for ignored_file in ignored_files {
        if let Some(ignored_file) = ignored_file.to_str()
            && whitelist.is_match(ignored_file)
        {
            continue;
        }
        exclusions.insert(ignored_file);
    }

    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use std::{
        collections::BTreeSet,
        path::{Path, PathBuf}, time::Duration,
    };

    use temp_dir_builder::{TempDirectory, TempDirectoryBuilder};

    use crate::{
        Logger,
        commands::{TimeMachineTrait, apply_diff_and_print, create_whitelist},
        config::Config,
        diff::Diff,
        timemachine::Error,
    };

    /// Create a Git repository with some files.
    ///
    /// When `root_directory` is None, the temporary directory is created in `/tmp`
    /// which is excluded from Time Machine backup, meaning all children files and directories
    /// will be considered excluded from Time Machine backup anyway (`tmutil isexcluded` will always returns "[Excluded]").
    pub(crate) fn create_repository(root_directory: Option<impl AsRef<Path>>) -> TempDirectory {
        let root_directory = root_directory.as_ref().map(|path| path.as_ref());
        if let Some(root_directory) = root_directory {
            if root_directory.exists() && root_directory.is_dir() {
                std::fs::remove_dir_all(&root_directory).unwrap();
            }
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

    pub(crate) fn create_config(search_directory: impl AsRef<Path>) -> Config {
        let mut config = Config::default();
        config.monitor_interval = Duration::from_secs(1);
        config.search_directories.clear();
        config
            .search_directories
            .insert(search_directory.as_ref().to_path_buf());

        config
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
        let mut logger = Logger::new(false);
        let diff = Diff {
            added: BTreeSet::from([temp_dir.path().join("a")]),
            removed: BTreeSet::new(),
        };
        let error_paths =
            apply_diff_and_print::<MockTimeMachineError>(&diff, false, false, &mut logger);

        assert_eq!(1, error_paths.len());
    }

    #[test]
    fn test_apply_diff_remove_print_error() {
        let temp_dir = TempDirectoryBuilder::default()
            .add_empty_file("a")
            .build()
            .unwrap();
        let mut logger = Logger::new(false);
        let diff = Diff {
            removed: BTreeSet::from([temp_dir.path().join("a")]),
            added: BTreeSet::new(),
        };
        let _ = apply_diff_and_print::<MockTimeMachineError>(&diff, false, false, &mut logger);
    }
}
