use std::collections::BTreeSet;

use log::info;

use crate::{
    cache::Cache,
    commands::TimeMachine,
    config::Config,
    diff::Exclusion,
    git::{self},
};

pub fn execute(
    config: &Config,
    cache: &mut Cache,
    dry_run: bool,
    details: bool,
) -> anyhow::Result<()> {
    let whitelist = super::create_whitelist(&config.whitelist_patterns)?;
    let mut repositories = BTreeSet::new();
    let mut exclusions: Vec<Exclusion> = Vec::new();

    info!("Searching for Git repositories...");
    if let Some((rx, thread_handle)) = git::find_repositories(
        &config.search_directories,
        &config.ignored_directories,
        config.threads,
    ) {
        while let Ok(repository_path) = rx.recv() {
            if repositories.insert(repository_path.clone()) {
                super::find_paths_to_exclude_from_backup(
                    repository_path,
                    &whitelist,
                    &mut exclusions,
                )?;
            }
        }

        super::join_thread(thread_handle)?;
        super::sort_and_dedup(&mut exclusions);

        info!(
            "Found {} {}",
            repositories.len(),
            crate::text::plural("repository", repositories.len())
        );

        if details {
            for repository in &repositories {
                info!("• {}", repository.display());
            }
        }

        let diff = cache.find_diff(&exclusions)?;

        let paths_failed_to_add =
            super::apply_diff_and_print::<TimeMachine>(&diff, dry_run, details);

        exclusions.retain(|exclusion| !paths_failed_to_add.contains(exclusion.path()));

        if !dry_run {
            cache.reset(exclusions)?;
        }
    }

    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use std::path::Path;

    use temp_dir_builder::TempDirectoryBuilder;

    use crate::cache::Cache;

    #[test]
    fn test_command() {
        let (temp_dir, a, b, c) = crate::commands::tests::create_repository(None);
        let mut cache = Cache::open_in_memory().unwrap();
        let config = crate::commands::tests::create_config(&temp_dir);
        let dry_run = false;
        super::execute(&config, &mut cache, dry_run, false).unwrap();
        let a_file_path = temp_dir.path_of(&a);
        let b_file_path = temp_dir.path_of(&b);
        let c_file_path = temp_dir.path_of(&c);
        let paths = cache.paths().unwrap();
        assert_eq!(2, paths.len());
        assert_eq!(a_file_path, paths[0]);
        assert_eq!(b_file_path, paths[1]);
        assert!(crate::timemachine::tests::is_excluded_from_time_machine(
            a_file_path
        ));
        assert!(crate::timemachine::tests::is_excluded_from_time_machine(
            b_file_path
        ));
        assert!(!crate::timemachine::tests::is_excluded_from_time_machine(
            c_file_path
        ));
    }

    #[test]
    fn test_command_with_a_relative_search_directory() {
        let directory_name = "test_run_relative_search_directory";
        let (_temp_dir, ..) =
            crate::commands::tests::create_repository(Some(Path::new(directory_name)));
        let mut cache = Cache::open_in_memory().unwrap();
        let config = crate::commands::tests::create_config(Path::new(directory_name));
        assert!(
            config
                .search_directories
                .iter()
                .all(|directory| directory.is_relative()),
            "the search directory must stay relative for this test to cover anything"
        );

        super::execute(&config, &mut cache, false, false).unwrap();

        let repository_path = std::env::current_dir().unwrap().join(directory_name);
        assert_eq!(
            vec![repository_path.join("a"), repository_path.join("b")],
            cache.paths().unwrap(),
            "a relative search directory must be resolved against the current directory, so the \
             exclusions it produces are absolute"
        );
    }

    #[test]
    fn test_gitignored_symlink_does_not_exclude_target() {
        let mut builder = TempDirectoryBuilder::default();
        builder.random_root_in(env!("CARGO_MANIFEST_DIR"));
        let precious = builder.add_text_file("outside/precious.txt", "precious data");
        let repository = builder.add_directory("repository");
        builder.in_directory(&repository, |builder| {
            builder.add_text_file(".gitignore", "link\n");
            builder.add_symlink_to("link", &precious);
        });
        let temp_dir = builder.build().unwrap();
        let repository_path = temp_dir.path_of(&repository);
        let target_path = temp_dir.path_of(&precious);
        crate::commands::tests::init_git_repository(&repository_path);
        let mut cache = Cache::open_in_memory().unwrap();
        let config = crate::commands::tests::create_config(&repository_path);

        assert!(!crate::timemachine::tests::is_excluded_from_time_machine(
            &target_path
        ));

        super::execute(&config, &mut cache, false, false).unwrap();

        assert!(
            !crate::timemachine::tests::is_excluded_from_time_machine(&target_path),
            "the Time Machine exclusion was applied to the target of a gitignored symlink, \
             outside the repository"
        );
    }

    #[test]
    fn test_case_only_rename_keeps_the_directory_excluded() {
        let mut builder = TempDirectoryBuilder::default();
        builder.random_root_in(env!("CARGO_MANIFEST_DIR"));
        let repository = builder.add_directory("repository");
        let foo_dir = builder.in_directory(&repository, |builder| {
            builder.add_text_file(".gitignore", "Foo\n");
            let foo_dir = builder.add_directory("Foo");
            builder.in_directory(&foo_dir, |builder| {
                builder.add_empty_file("a");
            });

            foo_dir
        });
        let temp_dir = builder.build().unwrap();
        let repository_path = temp_dir.path_of(&repository);
        crate::commands::tests::init_git_repository(&repository_path);

        let mut cache = Cache::open_in_memory().unwrap();
        let config = crate::commands::tests::create_config(&repository_path);

        super::execute(&config, &mut cache, false, false).unwrap();

        let upper_case_path = temp_dir.path_of(&foo_dir);
        assert!(
            crate::timemachine::tests::is_excluded_from_time_machine(&upper_case_path),
            "the initial scan should have excluded the ignored directory"
        );

        let lower_case_path = repository_path.join("foo");
        std::fs::rename(&upper_case_path, repository_path.join("renamed")).unwrap();
        std::fs::rename(repository_path.join("renamed"), &lower_case_path).unwrap();
        let mut additions = temp_dir.additions();
        additions.add_text_file("repository/.gitignore", "foo\n");
        additions.build().unwrap();

        super::execute(&config, &mut cache, false, false).unwrap();

        assert!(
            crate::timemachine::tests::is_excluded_from_time_machine(&lower_case_path),
            "the directory was renamed by case only so it is the same directory, and the \
             filesystem is case insensitive, so removing the old spelling must not undo the \
             exclusion of the new one"
        );
    }

    #[test]
    fn test_overlapping_search_directories_scan_every_repository_once() {
        let mut builder = TempDirectoryBuilder::default();
        builder.random_root_in(env!("CARGO_MANIFEST_DIR"));
        let nested = builder.add_directory("nested");
        let repository = builder.in_directory(&nested, |builder| {
            let repository = builder.add_directory("repository");
            builder.in_directory(&repository, |builder| {
                builder.add_text_file(".gitignore", "a\n");
                builder.add_empty_file("a");
                builder.add_empty_file("kept");
            });

            repository
        });
        let other_repository = builder.add_directory("other_repository");
        builder.in_directory(&other_repository, |builder| {
            builder.add_text_file(".gitignore", "b\n");
            builder.add_empty_file("b");
            builder.add_empty_file("kept");
        });
        let temp_dir = builder.build().unwrap();
        let nested_path = temp_dir.path_of(&nested);
        let repository_path = temp_dir.path_of(&repository);
        let other_repository_path = temp_dir.path_of(&other_repository);
        crate::commands::tests::init_git_repository(&repository_path);
        crate::commands::tests::init_git_repository(&other_repository_path);

        let mut config = crate::commands::tests::create_config(&temp_dir);
        config.search_directories.insert(nested_path);

        let mut cache = Cache::open_in_memory().unwrap();
        crate::commands::tests::FIND_PATHS_TO_EXCLUDE_CALLS.with(|calls| calls.set(0));

        super::execute(&config, &mut cache, false, false).unwrap();

        assert_eq!(
            2,
            crate::commands::tests::FIND_PATHS_TO_EXCLUDE_CALLS.with(std::cell::Cell::get),
            "'nested/repository' is reported by both overlapping search directories, so it was \
             scanned twice instead of once"
        );

        let mut expected = vec![repository_path.join("a"), other_repository_path.join("b")];
        expected.sort_unstable();
        let mut paths = cache.paths().unwrap();
        paths.sort_unstable();

        assert_eq!(
            expected, paths,
            "scanning each repository once dropped the exclusions of a repository"
        );
    }

    #[test]
    fn test_dry_run() {
        let (temp_dir, a, b, c) = crate::commands::tests::create_repository(None);
        let mut cache = Cache::open_in_memory().unwrap();
        let config = crate::commands::tests::create_config(&temp_dir);
        let dry_run = true;
        let a_file_path = temp_dir.path_of(&a);
        let b_file_path = temp_dir.path_of(&b);
        let c_file_path = temp_dir.path_of(&c);
        assert!(!crate::timemachine::tests::is_excluded_from_time_machine(
            &a_file_path
        ));
        assert!(!crate::timemachine::tests::is_excluded_from_time_machine(
            &b_file_path
        ));
        assert!(!crate::timemachine::tests::is_excluded_from_time_machine(
            &c_file_path
        ));
        super::execute(&config, &mut cache, dry_run, false).unwrap();
        assert_eq!(0, cache.paths().unwrap().len());
        assert!(!crate::timemachine::tests::is_excluded_from_time_machine(
            a_file_path
        ));
        assert!(!crate::timemachine::tests::is_excluded_from_time_machine(
            b_file_path
        ));
        assert!(!crate::timemachine::tests::is_excluded_from_time_machine(
            c_file_path
        ));
    }
}
