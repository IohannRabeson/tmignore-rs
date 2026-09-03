use std::{collections::BTreeSet, path::PathBuf};

use log::info;

use crate::{
    cache::Cache,
    commands::TimeMachine,
    config::Config,
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
    let mut exclusions: Vec<PathBuf> = Vec::new();

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
        exclusions.sort_unstable();
        exclusions.dedup();

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

        exclusions.retain(|path| !paths_failed_to_add.contains(path));

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
        let temp_dir =
            crate::commands::tests::create_repository(Some(Path::new("test_run_command")));
        let mut cache = Cache::open_in_memory().unwrap();
        let config = crate::commands::tests::create_config(temp_dir.path());
        let dry_run = false;
        super::execute(&config, &mut cache, dry_run, false).unwrap();
        let temp_dir_path = temp_dir.path().canonicalize().unwrap();
        let a_file_path = temp_dir_path.join("a");
        let b_file_path = temp_dir_path.join("b");
        let c_file_path = temp_dir_path.join("c");
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
    fn test_gitignored_symlink_does_not_exclude_target() {
        let root = crate::commands::tests::prepare_test_directory("test_run_gitignored_symlink");
        let temp_dir = TempDirectoryBuilder::default()
            .root_folder(&root)
            .add_text_file("outside/precious.txt", "precious data")
            .add_text_file("repository/.gitignore", "link\n")
            .build()
            .unwrap();
        let repository_path = temp_dir.path().join("repository");
        let target_path = temp_dir.path().join("outside").join("precious.txt");
        std::os::unix::fs::symlink(&target_path, repository_path.join("link")).unwrap();
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
        let root = crate::commands::tests::prepare_test_directory("test_run_case_only_rename");
        let temp_dir = TempDirectoryBuilder::default()
            .root_folder(&root)
            .add_text_file("repository/.gitignore", "Foo\n")
            .add_empty_file("repository/Foo/a")
            .build()
            .unwrap();
        let repository_path = temp_dir.path().join("repository");
        crate::commands::tests::init_git_repository(&repository_path);

        let mut cache = Cache::open_in_memory().unwrap();
        let config = crate::commands::tests::create_config(&repository_path);

        super::execute(&config, &mut cache, false, false).unwrap();

        let upper_case_path = repository_path.join("Foo");
        assert!(
            crate::timemachine::tests::is_excluded_from_time_machine(&upper_case_path),
            "the initial scan should have excluded the ignored directory"
        );

        let lower_case_path = repository_path.join("foo");
        std::fs::rename(&upper_case_path, repository_path.join("renamed")).unwrap();
        std::fs::rename(repository_path.join("renamed"), &lower_case_path).unwrap();
        std::fs::write(repository_path.join(".gitignore"), "foo\n").unwrap();

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
        let root = crate::commands::tests::prepare_test_directory("test_run_overlapping_search");
        let temp_dir = TempDirectoryBuilder::default()
            .root_folder(&root)
            .add_text_file("nested/repository/.gitignore", "a\n")
            .add_empty_file("nested/repository/a")
            .add_empty_file("nested/repository/kept")
            .add_text_file("other_repository/.gitignore", "b\n")
            .add_empty_file("other_repository/b")
            .add_empty_file("other_repository/kept")
            .build()
            .unwrap();
        let nested_path = temp_dir.path().join("nested");
        let repository_path = nested_path.join("repository");
        let other_repository_path = temp_dir.path().join("other_repository");
        crate::commands::tests::init_git_repository(&repository_path);
        crate::commands::tests::init_git_repository(&other_repository_path);

        let mut config = crate::commands::tests::create_config(temp_dir.path());
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

        let mut expected = vec![
            repository_path.canonicalize().unwrap().join("a"),
            other_repository_path.canonicalize().unwrap().join("b"),
        ];
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
        let temp_dir = crate::commands::tests::create_repository(Some(Path::new(
            "run_command_test_command_dry_run",
        )));
        let mut cache = Cache::open_in_memory().unwrap();
        let config = crate::commands::tests::create_config(temp_dir.path());
        let dry_run = true;
        let a_file_path = temp_dir.path().join("a");
        let b_file_path = temp_dir.path().join("b");
        let c_file_path = temp_dir.path().join("c");
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
