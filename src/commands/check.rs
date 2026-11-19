use std::{
    collections::BTreeSet,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};
use log::error;

use crate::{
    cache::Cache,
    commands::rules::{Excluded, Status, Whitelist},
    config::Config,
    timemachine,
};

fn resolve(
    path: &Path,
    search_directories: &BTreeSet<PathBuf>,
) -> std::io::Result<BTreeSet<PathBuf>> {
    let canonical_path = match (path.parent(), path.file_name()) {
        (Some(parent), Some(file_name)) if path.symlink_metadata()?.file_type().is_symlink() => {
            parent.canonicalize()?.join(file_name)
        }
        _ => path.canonicalize()?,
    };

    let spellings: BTreeSet<PathBuf> = search_directories
        .iter()
        .filter_map(|search_directory| {
            let canonical_search_directory = search_directory.canonicalize().ok()?;
            let relative_path = canonical_path
                .strip_prefix(canonical_search_directory)
                .ok()?;

            Some(
                search_directory
                    .iter()
                    .chain(relative_path.iter())
                    .collect(),
            )
        })
        .collect();

    Ok(if spellings.is_empty() {
        BTreeSet::from([canonical_path])
    } else {
        spellings
    })
}

pub fn execute(
    config: &Config,
    cache: &Cache,
    paths: &[PathBuf],
    writer: &mut impl Write,
) -> anyhow::Result<()> {
    let whitelist = Whitelist::new(&config.whitelist_patterns)?;
    let mut failure_count: usize = 0;

    for path in paths {
        if let Err(error) = check_path(config, cache, &whitelist, path, writer) {
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
    whitelist: &Whitelist,
    path: &Path,
    writer: &mut impl Write,
) -> anyhow::Result<()> {
    let path = std::path::absolute(path)?;
    let spellings = resolve(&path, &config.search_directories)
        .with_context(|| format!("Failed to read '{}'", path.display()))?;
    let mut checked = None;

    for spelling in spellings {
        let status = super::rules::status(&spelling, config, whitelist)?;
        let covering_path = cache.find_covering_path(&spelling)?;
        let found = matches!(status, Status::Excluded(_)) || covering_path.is_some();

        if found || checked.is_none() {
            checked = Some((spelling, status, covering_path));
        }
        if found {
            break;
        }
    }

    let Some((path, status, covering_path)) = checked else {
        bail!("Failed to resolve '{}'", path.display());
    };
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
    use rstest::rstest;

    use crate::cache::Cache;

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
    fn test_execute_with_a_search_directory_inside_an_ignored_directory_of_another() {
        let mut builder = crate::commands::tests::time_machine_visible_builder();
        builder.add_text_file("real/.gitignore", "build/\n");
        let file = builder.add_empty_file("real/build/file");
        let temp_dir = builder.build().unwrap();
        let repository_path = temp_dir.join("real");
        let link_path = temp_dir.join("link");
        let mut cache = Cache::open_in_memory().unwrap();
        let mut config = crate::commands::tests::create_config(&link_path);
        let mut output = Vec::new();

        std::os::unix::fs::symlink(&repository_path, &link_path).unwrap();
        crate::commands::tests::init_git_repository(&repository_path);
        config
            .search_directories
            .insert(repository_path.join("build"));
        crate::commands::run::execute(&config, &mut cache, false, false).unwrap();
        super::execute(&config, &cache, &[temp_dir.path_of(&file)], &mut output).unwrap();

        assert_eq!(
            format!(
                "{}\n  Rules: excluded by '{build}'\n  Exclusion list: listed through '{build}'\n  \
                 Time Machine: excluded\n",
                link_path.join("build/file").display(),
                build = link_path.join("build").display()
            ),
            String::from_utf8(output).unwrap(),
            "the full scan reaches the path only through the outer search directory, so the path \
             must be spelled through it"
        );
    }

    #[test]
    fn test_execute_in_a_whitelisted_repository_inside_a_directory_git_lists_as_ignored() {
        let mut builder = crate::commands::tests::time_machine_visible_builder();
        builder.add_text_file("outer/.gitignore", "vendor/*\n");
        builder.add_empty_file("outer/vendor/other");
        let kept = builder.add_empty_file("outer/vendor/dep/kept");
        let temp_dir = builder.build().unwrap();
        let outer_path = temp_dir.join("outer");
        let dependency_path = outer_path.join("vendor/dep");
        let mut cache = Cache::open_in_memory().unwrap();
        let mut config = crate::commands::tests::create_config(&outer_path);
        let mut output = Vec::new();

        config.whitelist_patterns.insert(String::from("*/dep"));
        crate::commands::tests::init_git_repository(&outer_path);
        crate::commands::tests::init_git_repository(&dependency_path);
        crate::commands::run::execute(&config, &mut cache, false, false).unwrap();
        super::execute(&config, &cache, &[temp_dir.path_of(&kept)], &mut output).unwrap();

        assert_eq!(
            format!(
                "{}\n  Rules: not excluded, the whitelist pattern '*/dep' matches '{}'\n  \
                 Exclusion list: not listed\n  Time Machine: not excluded\n",
                temp_dir.path_of(&kept).display(),
                dependency_path.display()
            ),
            String::from_utf8(output).unwrap(),
            "the whitelist keeps the repository, even though git also lists the directory holding \
             it as ignored"
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
