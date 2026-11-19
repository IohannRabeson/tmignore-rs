use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use log::error;
use regex::RegexSet;

use crate::{config::Config, diff::Exclusion, git};

pub struct Whitelist {
    patterns: RegexSet,
    globs: Vec<String>,
}

impl Whitelist {
    pub fn new(whitelist_patterns: &BTreeSet<String>) -> Result<Self, regex::Error> {
        let (globs, patterns): (Vec<String>, Vec<String>) = whitelist_patterns
            .iter()
            .filter_map(
                |pattern| match fnmatch_regex::glob_to_regex_pattern(pattern) {
                    Ok(compiled_pattern) => Some((pattern.clone(), compiled_pattern)),
                    Err(error) => {
                        error!("Error: invalid whitelist pattern '{pattern}': {error}");
                        None
                    }
                },
            )
            .unzip();

        Ok(Self {
            patterns: RegexSet::new(patterns)?,
            globs,
        })
    }

    fn matching_glob(&self, path: &Path) -> Option<&str> {
        let path = path.to_str()?;

        if !self.patterns.is_match(path) {
            return None;
        }

        let pattern_index = self.patterns.matches(path).iter().next()?;

        self.globs.get(pattern_index).map(String::as_str)
    }
}

enum SkipReason {
    Symlink,
    Whitelisted(String),
    HoldsWhitelisted(PathBuf),
}

fn skip_reason(ignored_file: &Path, whitelist: &Whitelist) -> Option<SkipReason> {
    if ignored_file
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Some(SkipReason::Symlink);
    }

    whitelist
        .matching_glob(ignored_file)
        .map(|glob| SkipReason::Whitelisted(glob.to_owned()))
}

fn find_ignored_files_with_skip_reasons(
    repository_path: &Path,
    whitelist: &Whitelist,
) -> anyhow::Result<Vec<(PathBuf, Option<SkipReason>)>> {
    let mut ignored_files: Vec<(PathBuf, Option<SkipReason>)> =
        git::find_ignored_files(repository_path)?
            .into_iter()
            .map(|ignored_file| {
                let skip_reason = skip_reason(&ignored_file, whitelist);

                (ignored_file, skip_reason)
            })
            .collect();
    let whitelisted_files: Vec<PathBuf> = ignored_files
        .iter()
        .filter(|(_, skip_reason)| matches!(skip_reason, Some(SkipReason::Whitelisted(_))))
        .map(|(ignored_file, _)| ignored_file.clone())
        .collect();

    for (ignored_file, skip_reason) in &mut ignored_files {
        if skip_reason.is_none()
            && let Some(whitelisted_file) = whitelisted_files.iter().find(|whitelisted_file| {
                whitelisted_file.starts_with(&*ignored_file) && *whitelisted_file != ignored_file
            })
        {
            *skip_reason = Some(SkipReason::HoldsWhitelisted(whitelisted_file.clone()));
        }
    }

    Ok(ignored_files)
}

/// Find the paths in a repository to exclude from Time Machine backup.
/// If a path matches at least one of the regexes in the `whitelist` `RegexSet` it will not be
/// added to `exclusions`. Paths may be pushed more than once; callers that need uniqueness
/// must dedup after collecting.
pub fn find_paths_to_exclude_from_backup(
    repository_path: impl AsRef<Path>,
    whitelist: &Whitelist,
    exclusions: &mut Vec<Exclusion>,
) -> anyhow::Result<Vec<PathBuf>> {
    #[cfg(test)]
    super::tests::FIND_PATHS_TO_EXCLUDE_CALLS.with(|calls| calls.set(calls.get() + 1));

    let repository_path = repository_path.as_ref();
    let mut whitelisted_directories = Vec::new();

    for (ignored_file, skip_reason) in
        find_ignored_files_with_skip_reasons(repository_path, whitelist)?
    {
        match skip_reason {
            Some(SkipReason::Whitelisted(_)) if ignored_file.is_dir() => {
                whitelisted_directories.push(ignored_file);
            }
            Some(_) => {}
            None => exclusions.push(Exclusion::new(ignored_file, repository_path.to_path_buf())),
        }
    }

    Ok(whitelisted_directories)
}

pub enum ScanSkipReason {
    OutsideSearchDirectories,
    InIgnoredDirectory(PathBuf),
}

pub fn scan_skip_reason(path: &Path, config: &Config) -> Option<ScanSkipReason> {
    let mut found_ignored_directory = None;

    for search_directory in config
        .search_directories
        .iter()
        .filter(|search_directory| path.starts_with(search_directory))
    {
        found_ignored_directory = Some(config.ignored_directories.iter().find(
            |ignored_directory| {
                path.starts_with(ignored_directory)
                    && ignored_directory.starts_with(search_directory)
            },
        )?);
    }

    Some(match found_ignored_directory {
        Some(ignored_directory) => ScanSkipReason::InIgnoredDirectory(ignored_directory.clone()),
        None => ScanSkipReason::OutsideSearchDirectories,
    })
}

#[derive(Debug, PartialEq)]
pub enum Excluded {
    Directly,
    ByAncestor(PathBuf),
}

impl Excluded {
    pub fn by(covering_path: PathBuf, path: &Path) -> Self {
        if covering_path == path {
            Excluded::Directly
        } else {
            Excluded::ByAncestor(covering_path)
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum NotExcluded {
    NotIgnored,
    Whitelisted { pattern: String, path: PathBuf },
    Symlink,
    NoRepository,
    InIgnoredDirectory(PathBuf),
    HoldsWhitelisted(PathBuf),
}

#[derive(Debug, PartialEq)]
pub enum Status {
    Excluded(Excluded),
    NotExcluded(NotExcluded),
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Status::Excluded(Excluded::Directly) => write!(f, "excluded"),
            Status::Excluded(Excluded::ByAncestor(ancestor)) => {
                write!(f, "excluded by '{}'", ancestor.display())
            }
            Status::NotExcluded(NotExcluded::NotIgnored) => {
                write!(f, "not excluded, Git does not ignore it")
            }
            Status::NotExcluded(NotExcluded::Whitelisted { pattern, path }) => write!(
                f,
                "not excluded, the whitelist pattern '{pattern}' matches '{}'",
                path.display()
            ),
            Status::NotExcluded(NotExcluded::Symlink) => {
                write!(f, "not excluded, symbolic links are never excluded")
            }
            Status::NotExcluded(NotExcluded::NoRepository) => write!(
                f,
                "not excluded, it is not in a Git repository of the search directories"
            ),
            Status::NotExcluded(NotExcluded::InIgnoredDirectory(ignored_directory)) => write!(
                f,
                "not excluded, it is in the ignored directory '{}'",
                ignored_directory.display()
            ),
            Status::NotExcluded(NotExcluded::HoldsWhitelisted(whitelisted_file)) => write!(
                f,
                "not excluded, it holds '{}' the whitelist keeps",
                whitelisted_file.display()
            ),
        }
    }
}

pub fn status(path: &Path, config: &Config, whitelist: &Whitelist) -> anyhow::Result<Status> {
    let mut repository_paths = Vec::new();
    let mut skipping_ignored_directory = None;
    let mut current = Some(path.to_path_buf());

    while let Some(repository_path) = current.and_then(git::find_parent_repository) {
        current = repository_path.parent().map(Path::to_path_buf);

        match scan_skip_reason(&repository_path, config) {
            None => repository_paths.push(repository_path),
            Some(ScanSkipReason::InIgnoredDirectory(ignored_directory)) => {
                skipping_ignored_directory = Some(ignored_directory);
            }
            Some(ScanSkipReason::OutsideSearchDirectories) => {}
        }
    }

    let mut not_excluded = match (repository_paths.is_empty(), skipping_ignored_directory) {
        (_, Some(ignored_directory)) => NotExcluded::InIgnoredDirectory(ignored_directory),
        (false, None) => NotExcluded::NotIgnored,
        (true, None) => NotExcluded::NoRepository,
    };

    for repository_path in repository_paths.iter().rev() {
        for (ignored_file, skip_reason) in
            find_ignored_files_with_skip_reasons(repository_path, whitelist)?
        {
            if !path.starts_with(&ignored_file) {
                continue;
            }

            match skip_reason {
                Some(SkipReason::Symlink) => not_excluded = NotExcluded::Symlink,
                Some(SkipReason::HoldsWhitelisted(whitelisted_file)) => {
                    not_excluded = NotExcluded::HoldsWhitelisted(whitelisted_file);
                }
                Some(SkipReason::Whitelisted(pattern)) => {
                    not_excluded = NotExcluded::Whitelisted {
                        pattern,
                        path: ignored_file,
                    };
                }
                None => return Ok(Status::Excluded(Excluded::by(ignored_file, path))),
            }
        }
    }

    Ok(Status::NotExcluded(not_excluded))
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, path::Path};

    use rstest::rstest;
    use temp_dir_builder::TempDirectoryBuilder;

    use super::{Excluded, NotExcluded, Status, Whitelist};

    #[test]
    fn test_create_whitelist_invalid() {
        let patterns = BTreeSet::from([String::from("[z-a].txt")]);
        let result = Whitelist::new(&patterns).unwrap();
        assert!(result.patterns.is_empty());
    }

    #[rstest]
    #[case::ignored_file("repository/ignored", |_: &Path| Status::Excluded(Excluded::Directly))]
    #[case::in_an_ignored_directory(
        "repository/build/artifact",
        |repository: &Path| Status::Excluded(Excluded::ByAncestor(repository.join("build")))
    )]
    #[case::not_ignored("repository/kept", |_: &Path| Status::NotExcluded(NotExcluded::NotIgnored))]
    #[case::whitelisted(
        "repository/whitelisted",
        |repository: &Path| Status::NotExcluded(NotExcluded::Whitelisted {
            pattern: String::from("*/whitelisted"),
            path: repository.join("whitelisted"),
        })
    )]
    #[case::in_a_whitelisted_directory(
        "repository/kept_directory/file",
        |repository: &Path| Status::NotExcluded(NotExcluded::Whitelisted {
            pattern: String::from("*/kept_directory"),
            path: repository.join("kept_directory"),
        })
    )]
    #[case::symlink("repository/link", |_: &Path| Status::NotExcluded(NotExcluded::Symlink))]
    #[case::outside_any_repository("outside", |_: &Path| Status::NotExcluded(NotExcluded::NoRepository))]
    fn test_status(#[case] relative_path: &str, #[case] expected: fn(&Path) -> Status) {
        let mut builder = TempDirectoryBuilder::default();
        let outside = builder.add_empty_file("outside");
        let repository = builder.add_directory("repository");
        builder.in_directory(&repository, |builder| {
            builder.add_text_file(
                ".gitignore",
                "ignored\nbuild/\nwhitelisted\nkept_directory/\nlink\n",
            );
            builder.add_empty_file("ignored");
            builder.add_empty_file("kept_directory/file");
            builder.add_empty_file("build/artifact");
            builder.add_empty_file("kept");
            builder.add_empty_file("whitelisted");
            builder.add_symlink_to("link", &outside);
        });
        let temp_dir = builder.build().unwrap();
        let repository_path = temp_dir.path_of(&repository);
        let config = crate::commands::tests::create_config(&repository_path);
        let whitelist = Whitelist::new(&BTreeSet::from([
            String::from("![z-a]"),
            String::from("*/kept_directory"),
            String::from("*/whitelisted"),
        ]))
        .unwrap();

        crate::commands::tests::init_git_repository(&repository_path);

        assert_eq!(
            2,
            whitelist.patterns.len(),
            "the first pattern must be invalid, so the position of a pattern in the configuration \
             differs from its position in the compiled whitelist"
        );
        assert_eq!(
            expected(&repository_path),
            super::status(&temp_dir.join(relative_path), &config, &whitelist).unwrap()
        );
    }

    #[test]
    fn test_status_in_a_nested_repository_ignored_by_the_outer_one() {
        let mut builder = TempDirectoryBuilder::default();
        builder.add_text_file("outer/.gitignore", "nested/\n");
        let kept = builder.add_empty_file("outer/nested/kept");
        let temp_dir = builder.build().unwrap();
        let outer_path = temp_dir.join("outer");
        let nested_path = outer_path.join("nested");
        let config = crate::commands::tests::create_config(&outer_path);
        let whitelist = Whitelist::new(&BTreeSet::new()).unwrap();

        crate::commands::tests::init_git_repository(&outer_path);
        crate::commands::tests::init_git_repository(&nested_path);

        assert_eq!(
            Status::Excluded(Excluded::ByAncestor(nested_path)),
            super::status(&temp_dir.path_of(&kept), &config, &whitelist).unwrap(),
            "the nested repository does not ignore the file, but the outer one ignores the whole \
             nested repository"
        );
    }

    #[rstest]
    #[case::in_the_ignored_directory("ignored/repository", false)]
    #[case::is_the_ignored_directory("ignored", false)]
    #[case::in_an_outer_repository_holding_the_ignored_directory("ignored/repository", true)]
    fn test_status_in_a_repository_the_full_scan_skips(
        #[case] relative_repository_path: &str,
        #[case] in_an_outer_repository: bool,
    ) {
        let temp_dir = TempDirectoryBuilder::default().build().unwrap();
        let repository_path = temp_dir.join(relative_repository_path);
        let ignored_directory = temp_dir.join("ignored");
        let file_path = repository_path.join("file");

        if in_an_outer_repository {
            crate::commands::tests::init_git_repository(temp_dir.path());
        }
        crate::commands::tests::init_git_repository(&repository_path);
        std::fs::write(repository_path.join(".gitignore"), "file\n").unwrap();
        std::fs::write(&file_path, "").unwrap();

        let mut config = crate::commands::tests::create_config(&temp_dir);
        let whitelist = Whitelist::new(&BTreeSet::new()).unwrap();

        config.ignored_directories.insert(ignored_directory.clone());

        assert_eq!(
            Status::NotExcluded(NotExcluded::InIgnoredDirectory(ignored_directory)),
            super::status(&file_path, &config, &whitelist).unwrap(),
            "the full scan does not look for repositories in the ignored directories, so what \
             their .gitignore files say does not apply"
        );
    }

    #[test]
    fn test_status_in_a_search_directory_inside_an_ignored_directory() {
        let temp_dir = TempDirectoryBuilder::default().build().unwrap();
        let search_directory = temp_dir.join("search");
        let repository_path = search_directory.join("repository");
        let file_path = repository_path.join("file");

        crate::commands::tests::init_git_repository(&repository_path);
        std::fs::write(repository_path.join(".gitignore"), "file\n").unwrap();
        std::fs::write(&file_path, "").unwrap();

        let mut config = crate::commands::tests::create_config(&search_directory);
        let whitelist = Whitelist::new(&BTreeSet::new()).unwrap();

        config
            .ignored_directories
            .insert(temp_dir.path().to_path_buf());

        assert_eq!(
            Status::Excluded(Excluded::Directly),
            super::status(&file_path, &config, &whitelist).unwrap(),
            "the full scan only skips the ignored directories it walks through, so it scans the \
             repositories of a search directory inside an ignored directory"
        );
    }
}
