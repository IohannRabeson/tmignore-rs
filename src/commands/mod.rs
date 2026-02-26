pub mod run;
pub mod list;
pub mod reset;
pub mod monitor;

use std::{collections::BTreeSet, path::Path};

use regex::RegexSet;

use crate::{Logger, git, timemachine};

struct ApplyError<'a> {
    error: std::io::Error,
    path: &'a Path,
    added: bool,
}

fn apply_diff_and_print<'a>(
    diff: &'a crate::diff::Diff,
    dry_run: bool,
    details: bool,
    logger: &mut Logger,
) -> Vec<&'a Path> {
    let mut errors = Vec::new();
    let mut add_failed_paths = BTreeSet::new();

    for path in &diff.added {
        if !dry_run && let Err(error) = timemachine::add_exclusion(path) {
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
            && let Err(error) = timemachine::remove_exclusion(path)
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