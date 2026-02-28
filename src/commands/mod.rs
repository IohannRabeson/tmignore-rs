pub mod list;
pub mod monitor;
pub mod reset;
pub mod run;

use std::{collections::BTreeSet, path::Path};

use regex::RegexSet;

use crate::{Logger, git, timemachine};

struct ApplyError<'a> {
    error: std::io::Error,
    path: &'a Path,
    added: bool,
}

trait TimeMachineTrait {
    fn add_exclusion(path: impl AsRef<Path>) -> Result<(), std::io::Error>;
    fn remove_exclusion(path: impl AsRef<Path>) -> Result<(), std::io::Error>;
}

struct TimeMachine;

impl TimeMachineTrait for TimeMachine {
    fn add_exclusion(path: impl AsRef<Path>) -> Result<(), std::io::Error> {
        timemachine::add_exclusion(path)
    }

    fn remove_exclusion(path: impl AsRef<Path>) -> Result<(), std::io::Error> {
        timemachine::remove_exclusion(path)
    }
}

/// Calls TM::add_exclusion and TM::remove_exclusion depending on the diff.
/// Returns the list of paths failed to be added.
fn apply_diff_and_print<'a, TM: TimeMachineTrait>(
    diff: &'a crate::diff::Diff,
    dry_run: bool,
    details: bool,
    logger: &mut Logger,
) -> Vec<&'a Path> {
    let mut errors = Vec::new();
    let mut add_failed_paths = BTreeSet::new();

    for path in &diff.added {
        if !dry_run && let Err(error) = TM::add_exclusion(path) {
            add_failed_paths.insert(path);
            errors.push(ApplyError {
                error,
                path,
                added: true,
            });
        }
    }

    for path in &diff.removed {
        if !dry_run
            && path.exists()
            && let Err(error) = TM::remove_exclusion(path)
        {
            errors.push(ApplyError {
                error,
                path,
                added: false,
            });
        }
    }

    let add_count = diff.added.len() - add_failed_paths.len();
    let remove_count = diff.removed.len();

    if add_count > 0 {
        logger.log(format!(
            "Added {} paths to the backup exclusion list",
            add_count
        ));
    }

    if remove_count > 0 {
        logger.log(format!(
            "Removed {} paths from the backup exclusion list",
            remove_count
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

    for error in &errors {
        eprintln!("Error: {}: {}", error.path.display(), error.error);
    }

    errors
        .into_iter()
        .filter(|error| error.added)
        .map(|entry| entry.path)
        .collect()
}

fn create_whitelist(whitelist_patterns: &BTreeSet<String>) -> Result<RegexSet, regex::Error> {
    RegexSet::new(whitelist_patterns.iter().filter_map(|pattern| {
        match fnmatch_regex::glob_to_regex_pattern(pattern) {
            Ok(pattern) => Some(pattern),
            Err(error) => {
                eprintln!("Error: invalid whitelist pattern '{}': {}", pattern, error);
                None
            }
        }
    }))
}

/// Find the paths in a repository to exclude from Time Machine backup.
/// If a path matches at least one of the regexes in the `whitelist` RegexSet it will not be
/// added to the `exclusion` set.
fn find_paths_to_exclude_from_backup(
    repository_path: impl AsRef<Path>,
    whitelist: &RegexSet,
    exclusions: &mut BTreeSet<std::path::PathBuf>,
) -> Result<(), git::FindIgnoredFileError> {
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
    use std::{collections::BTreeSet, path::Path};

    use temp_dir_builder::{TempDirectory, TempDirectoryBuilder};

    use crate::{
        Logger,
        commands::{TimeMachineTrait, apply_diff_and_print, create_whitelist},
        config::Config,
        diff::Diff,
    };

    pub(crate) fn create_repository(root_directory: impl AsRef<Path>) -> TempDirectory {
        let root_directory = root_directory.as_ref();
        if root_directory.exists() && root_directory.is_dir() {
            std::fs::remove_dir_all(&root_directory).unwrap();
        }
        let temp_dir = TempDirectoryBuilder::default()
            .root_folder(root_directory)
            .add_text_file(".gitignore", "a\nb\n")
            .add_empty_file("a")
            .add_empty_file("b")
            .add_empty_file("c")
            .build()
            .unwrap();

        std::process::Command::new("git")
            .arg("init")
            .arg(temp_dir.path())
            .output()
            .unwrap();

        temp_dir
    }

    pub(crate) fn create_config(search_directory: impl AsRef<Path>) -> Config {
        let mut config = Config::default();
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
        fn add_exclusion(_path: impl AsRef<Path>) -> Result<(), std::io::Error> {
            Err(std::io::Error::new(std::io::ErrorKind::StorageFull, "fail"))
        }

        fn remove_exclusion(_path: impl AsRef<Path>) -> Result<(), std::io::Error> {
            Err(std::io::Error::new(std::io::ErrorKind::StorageFull, "fail"))
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
