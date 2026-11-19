use std::{
    collections::BTreeSet,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};
use log::error;
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
    InIgnoredDirectory(PathBuf),
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
        }
    }
}

fn status(
    path: &Path,
    search_directories: &BTreeSet<PathBuf>,
    ignored_directories: &BTreeSet<PathBuf>,
    whitelist_globs: &[&String],
    whitelist: &RegexSet,
) -> anyhow::Result<Status> {
    let mut repository_paths = Vec::new();
    let mut skipping_ignored_directory = None;
    let mut current = Some(path.to_path_buf());

    while let Some(repository_path) = current.and_then(git::find_parent_repository) {
        current = repository_path.parent().map(Path::to_path_buf);

        if search_directories
            .iter()
            .any(|search_directory| repository_path.starts_with(search_directory))
        {
            match super::find_ignored_directory(
                &repository_path,
                search_directories,
                ignored_directories,
            ) {
                Some(ignored_directory) => skipping_ignored_directory = Some(ignored_directory),
                None => repository_paths.push(repository_path),
            }
        }
    }

    let mut not_excluded = match (repository_paths.is_empty(), skipping_ignored_directory) {
        (_, Some(ignored_directory)) => NotExcluded::InIgnoredDirectory(ignored_directory.clone()),
        (false, None) => NotExcluded::NotIgnored,
        (true, None) => NotExcluded::NoRepository,
    };

    for repository_path in repository_paths.iter().rev() {
        for ignored_file in git::find_ignored_files(repository_path)? {
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

    Ok(Status::NotExcluded(not_excluded))
}

fn resolve(path: &Path, search_directories: &BTreeSet<PathBuf>) -> std::io::Result<PathBuf> {
    let canonical_path = match (path.parent(), path.file_name()) {
        (Some(parent), Some(file_name)) if path.symlink_metadata()?.file_type().is_symlink() => {
            parent.canonicalize()?.join(file_name)
        }
        _ => path.canonicalize()?,
    };

    let innermost = search_directories
        .iter()
        .filter_map(|search_directory| {
            let canonical_search_directory = search_directory.canonicalize().ok()?;
            let relative_path = canonical_path
                .strip_prefix(canonical_search_directory)
                .ok()?
                .to_path_buf();

            Some((search_directory, relative_path))
        })
        .min_by_key(|(_, relative_path)| relative_path.components().count());

    Ok(match innermost {
        Some((search_directory, relative_path)) => search_directory
            .iter()
            .chain(relative_path.iter())
            .collect(),
        None => canonical_path,
    })
}

pub fn execute(
    config: &Config,
    cache: &Cache,
    paths: &[PathBuf],
    writer: &mut impl Write,
) -> anyhow::Result<()> {
    let (whitelist, whitelist_globs) =
        super::create_whitelist_with_globs(&config.whitelist_patterns)?;
    let mut failure_count: usize = 0;

    for path in paths {
        if let Err(error) = check_path(config, cache, &whitelist_globs, &whitelist, path, writer) {
            error!("Error: {error:#}");
            failure_count = failure_count.saturating_add(1);
        }
    }

    if failure_count > 0 {
        bail!(
            "Failed to check {failure_count} {}",
            crate::text::plural("path", failure_count)
        );
    }

    Ok(())
}

fn check_path(
    config: &Config,
    cache: &Cache,
    whitelist_globs: &[&String],
    whitelist: &RegexSet,
    path: &Path,
    writer: &mut impl Write,
) -> anyhow::Result<()> {
    let path = std::path::absolute(path)?;
    let path = resolve(&path, &config.search_directories)
        .with_context(|| format!("Failed to read '{}'", path.display()))?;

    let status = status(
        &path,
        &config.search_directories,
        &config.ignored_directories,
        whitelist_globs,
        whitelist,
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
                &BTreeSet::new(),
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
                &BTreeSet::new(),
                &whitelist_globs,
                &whitelist
            )
            .unwrap(),
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

        let search_directories = BTreeSet::from([temp_dir.path().to_path_buf()]);
        let ignored_directories = BTreeSet::from([ignored_directory.clone()]);
        let whitelist_patterns = BTreeSet::new();
        let (whitelist, whitelist_globs) =
            crate::commands::create_whitelist_with_globs(&whitelist_patterns).unwrap();

        assert_eq!(
            Status::NotExcluded(NotExcluded::InIgnoredDirectory(ignored_directory)),
            super::status(
                &file_path,
                &search_directories,
                &ignored_directories,
                &whitelist_globs,
                &whitelist
            )
            .unwrap(),
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

        let search_directories = BTreeSet::from([search_directory]);
        let ignored_directories = BTreeSet::from([temp_dir.path().to_path_buf()]);
        let whitelist_patterns = BTreeSet::new();
        let (whitelist, whitelist_globs) =
            crate::commands::create_whitelist_with_globs(&whitelist_patterns).unwrap();

        assert_eq!(
            Status::Excluded(Excluded::Directly),
            super::status(
                &file_path,
                &search_directories,
                &ignored_directories,
                &whitelist_globs,
                &whitelist
            )
            .unwrap(),
            "the full scan only skips the ignored directories it walks through, so it scans the \
             repositories of a search directory inside an ignored directory"
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

    #[rstest]
    #[case::other_case("A")]
    #[case::through_a_symlink("link_to_root/a")]
    fn test_execute_with_another_spelling_of_the_path(#[case] spelling: &str) {
        let (temp_dir, a, ..) = crate::commands::tests::create_repository(None);
        let mut cache = Cache::open_in_memory().unwrap();
        let config = crate::commands::tests::create_config(&temp_dir);
        let mut output = Vec::new();

        std::os::unix::fs::symlink(temp_dir.path(), temp_dir.join("link_to_root")).unwrap();
        crate::commands::run::execute(&config, &mut cache, false, false).unwrap();
        super::execute(&config, &cache, &[temp_dir.join(spelling)], &mut output).unwrap();

        assert_eq!(
            format!(
                "{}\n  Rules: excluded\n  Exclusion list: listed\n  Time Machine: excluded\n",
                temp_dir.path_of(&a).display()
            ),
            String::from_utf8(output).unwrap()
        );
    }

    #[test]
    fn test_execute_reports_the_paths_after_a_path_that_cannot_be_read() {
        let (temp_dir, a, _, c) = crate::commands::tests::create_repository(None);
        let cache = Cache::open_in_memory().unwrap();
        let config = crate::commands::tests::create_config(&temp_dir);
        let a_path = temp_dir.path_of(&a);
        let c_path = temp_dir.path_of(&c);
        let mut output = Vec::new();

        let result = super::execute(
            &config,
            &cache,
            &[a_path.clone(), temp_dir.join("missing"), c_path.clone()],
            &mut output,
        );

        assert!(
            result.is_err(),
            "a path could not be read, so the command must fail"
        );
        assert_eq!(
            format!(
                "{}\n  Rules: excluded\n  Exclusion list: not listed\n  Time Machine: not excluded\n\
                 {}\n  Rules: not excluded, Git does not ignore it\n  Exclusion list: not listed\n  \
                 Time Machine: not excluded\n",
                a_path.display(),
                c_path.display()
            ),
            String::from_utf8(output).unwrap(),
            "a path that cannot be read must not prevent the paths after it from being checked"
        );
    }

    #[test]
    fn test_execute_in_a_nested_repository_ignored_by_the_outer_one_and_whitelisted() {
        let mut builder = crate::commands::tests::time_machine_visible_builder();
        builder.add_text_file("outer/.gitignore", "nested/\n");
        builder.add_text_file("outer/nested/.gitignore", "build/\n");
        let artifact = builder.add_empty_file("outer/nested/build/artifact");
        let temp_dir = builder.build().unwrap();
        let outer_path = temp_dir.join("outer");
        let nested_path = outer_path.join("nested");
        let mut cache = Cache::open_in_memory().unwrap();
        let mut config = crate::commands::tests::create_config(&outer_path);
        let mut output = Vec::new();

        config.whitelist_patterns.insert(String::from("*/nested"));
        crate::commands::tests::init_git_repository(&outer_path);
        crate::commands::tests::init_git_repository(&nested_path);
        crate::commands::run::execute(&config, &mut cache, false, false).unwrap();
        super::execute(&config, &cache, &[temp_dir.path_of(&artifact)], &mut output).unwrap();

        assert_eq!(
            format!(
                "{}\n  Rules: excluded by '{build}'\n  Exclusion list: listed through '{build}'\n  \
                 Time Machine: excluded\n",
                temp_dir.path_of(&artifact).display(),
                build = nested_path.join("build").display()
            ),
            String::from_utf8(output).unwrap(),
            "the whitelist keeps the nested repository the outer one ignores in the backup, so \
             the nested repository decides for what it holds"
        );
    }

    #[test]
    fn test_execute_with_a_search_directory_inside_another_given_through_a_symlink() {
        let mut builder = crate::commands::tests::time_machine_visible_builder();
        builder.add_text_file("real/repository/.gitignore", "ignored\n");
        let ignored = builder.add_empty_file("real/repository/ignored");
        let temp_dir = builder.build().unwrap();
        let repository_path = temp_dir.join("real/repository");
        let link_path = temp_dir.join("a_link");
        let mut cache = Cache::open_in_memory().unwrap();
        let mut config = crate::commands::tests::create_config(&repository_path);
        let mut output = Vec::new();

        std::os::unix::fs::symlink(temp_dir.join("real"), &link_path).unwrap();
        crate::commands::tests::init_git_repository(&repository_path);
        config.search_directories.insert(link_path.clone());
        config
            .ignored_directories
            .insert(link_path.join("repository"));
        crate::commands::run::execute(&config, &mut cache, false, false).unwrap();
        super::execute(&config, &cache, &[temp_dir.path_of(&ignored)], &mut output).unwrap();

        assert_eq!(
            format!(
                "{}\n  Rules: excluded\n  Exclusion list: listed\n  Time Machine: excluded\n",
                temp_dir.path_of(&ignored).display()
            ),
            String::from_utf8(output).unwrap(),
            "the full scan reaches the path only through the innermost search directory, so the \
             path must be spelled through it"
        );
    }

    #[test]
    fn test_execute_with_a_search_directory_given_through_a_symlink() {
        let (temp_dir, ..) = crate::commands::tests::create_repository(None);
        let mut cache = Cache::open_in_memory().unwrap();
        let link_path = temp_dir.join("link_to_root");
        let config = crate::commands::tests::create_config(&link_path);
        let mut output = Vec::new();

        std::os::unix::fs::symlink(temp_dir.path(), &link_path).unwrap();
        crate::commands::run::execute(&config, &mut cache, false, false).unwrap();
        super::execute(&config, &cache, &[temp_dir.join("a")], &mut output).unwrap();

        assert_eq!(
            format!(
                "{}\n  Rules: excluded\n  Exclusion list: listed\n  Time Machine: excluded\n",
                link_path.join("a").display()
            ),
            String::from_utf8(output).unwrap()
        );
    }
}
