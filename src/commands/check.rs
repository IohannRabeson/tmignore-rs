use std::{
    collections::BTreeSet,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::Context;
use regex::RegexSet;

use crate::{cache::Cache, commands::SkipReason, config::Config, git, timemachine};

#[derive(Debug, PartialEq)]
pub enum Excluded {
    Directly,
    ByAncestor(PathBuf),
}

impl Excluded {
    fn by(covering_path: PathBuf, path: &Path) -> Self {
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
        }
    }
}

fn status(
    path: &Path,
    search_directories: &BTreeSet<PathBuf>,
    whitelist_globs: &[&String],
    whitelist: &RegexSet,
) -> anyhow::Result<Status> {
    let mut not_excluded = NotExcluded::NoRepository;
    let mut current = Some(path.to_path_buf());

    while let Some(repository_path) = current.and_then(git::find_parent_repository) {
        if search_directories
            .iter()
            .any(|search_directory| repository_path.starts_with(search_directory))
        {
            if not_excluded == NotExcluded::NoRepository {
                not_excluded = NotExcluded::NotIgnored;
            }

            for ignored_file in git::find_ignored_files(&repository_path)? {
                if !path.starts_with(&ignored_file) {
                    continue;
                }

                match super::skip_reason(&ignored_file, whitelist) {
                    Some(SkipReason::Symlink) => not_excluded = NotExcluded::Symlink,
                    Some(SkipReason::Whitelisted(pattern_index)) => {
                        not_excluded = NotExcluded::Whitelisted {
                            pattern: whitelist_globs
                                .get(pattern_index)
                                .copied()
                                .cloned()
                                .unwrap_or_default(),
                            path: ignored_file,
                        };
                    }
                    None => return Ok(Status::Excluded(Excluded::by(ignored_file, path))),
                }
            }
        }

        current = repository_path.parent().map(Path::to_path_buf);
    }

    Ok(Status::NotExcluded(not_excluded))
}

pub fn execute(
    config: &Config,
    cache: &Cache,
    paths: &[PathBuf],
    writer: &mut impl Write,
) -> anyhow::Result<()> {
    let (whitelist, whitelist_globs) =
        super::create_whitelist_with_globs(&config.whitelist_patterns)?;

    for path in paths {
        let path = std::path::absolute(path)?;

        std::fs::symlink_metadata(&path)
            .with_context(|| format!("Failed to read '{}'", path.display()))?;

        let status = status(
            &path,
            &config.search_directories,
            &whitelist_globs,
            &whitelist,
        )?;
        let covering_path = cache.find_covering_path(&path)?;
        let listed = match covering_path.map(|covering_path| Excluded::by(covering_path, &path)) {
            Some(Excluded::Directly) => String::from("listed"),
            Some(Excluded::ByAncestor(ancestor)) => {
                format!("listed through '{}'", ancestor.display())
            }
            None => String::from("not listed"),
        };
        let time_machine = if timemachine::is_excluded(&path)? {
            "excluded"
        } else {
            "not excluded"
        };

        writeln!(writer, "{}", path.display())?;
        writeln!(writer, "  Rules: {status}")?;
        writeln!(writer, "  Exclusion list: {listed}")?;
        writeln!(writer, "  Time Machine: {time_machine}")?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, path::Path};

    use rstest::rstest;
    use temp_dir_builder::TempDirectoryBuilder;

    use super::{Excluded, NotExcluded, Status};
    use crate::cache::Cache;

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
        let search_directories = BTreeSet::from([repository_path.clone()]);
        let whitelist_patterns = BTreeSet::from([
            String::from("![z-a]"),
            String::from("*/kept_directory"),
            String::from("*/whitelisted"),
        ]);
        let (whitelist, whitelist_globs) =
            crate::commands::create_whitelist_with_globs(&whitelist_patterns).unwrap();

        crate::commands::tests::init_git_repository(&repository_path);

        assert_eq!(
            2,
            whitelist.len(),
            "the first pattern must be invalid, so the position of a pattern in the configuration \
             differs from its position in the compiled whitelist"
        );
        assert_eq!(
            expected(&repository_path),
            super::status(
                &temp_dir.join(relative_path),
                &search_directories,
                &whitelist_globs,
                &whitelist
            )
            .unwrap()
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
        let search_directories = BTreeSet::from([outer_path.clone()]);
        let whitelist_patterns = BTreeSet::new();
        let (whitelist, whitelist_globs) =
            crate::commands::create_whitelist_with_globs(&whitelist_patterns).unwrap();

        crate::commands::tests::init_git_repository(&outer_path);
        crate::commands::tests::init_git_repository(&nested_path);

        assert_eq!(
            Status::Excluded(Excluded::ByAncestor(nested_path)),
            super::status(
                &temp_dir.path_of(&kept),
                &search_directories,
                &whitelist_globs,
                &whitelist
            )
            .unwrap(),
            "the nested repository does not ignore the file, but the outer one ignores the whole \
             nested repository"
        );
    }

    #[test]
    fn test_execute() {
        let (temp_dir, a, ..) = crate::commands::tests::create_repository(None);
        let mut cache = Cache::open_in_memory().unwrap();
        let config = crate::commands::tests::create_config(&temp_dir);
        let a_file_path = temp_dir.path_of(&a);
        let mut before_scan = Vec::new();
        let mut after_scan = Vec::new();

        super::execute(
            &config,
            &cache,
            std::slice::from_ref(&a_file_path),
            &mut before_scan,
        )
        .unwrap();
        crate::commands::run::execute(&config, &mut cache, false, false).unwrap();
        super::execute(
            &config,
            &cache,
            std::slice::from_ref(&a_file_path),
            &mut after_scan,
        )
        .unwrap();

        assert_eq!(
            format!(
                "{}\n  Rules: excluded\n  Exclusion list: not listed\n  Time Machine: not excluded\n",
                a_file_path.display()
            ),
            String::from_utf8(before_scan).unwrap()
        );
        assert_eq!(
            format!(
                "{}\n  Rules: excluded\n  Exclusion list: listed\n  Time Machine: excluded\n",
                a_file_path.display()
            ),
            String::from_utf8(after_scan).unwrap()
        );
    }
}
