use std::collections::BTreeSet;

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
    let mut exclusions = BTreeSet::new();

    info!("Searching for Git repositories...");
    if let Some((rx, thread_handle)) = git::find_repositories(
        &config.search_directories,
        &config.ignored_directories,
        config.threads,
    ) {
        while let Ok(repository_path) = rx.recv() {
            repositories.insert(repository_path.clone());

            super::find_paths_to_exclude_from_backup(repository_path, &whitelist, &mut exclusions)?;
        }
        thread_handle.join().unwrap();

        info!("Found {} repositories", repositories.len());

        if details {
            for repository in &repositories {
                info!(" • {}", repository.display());
            }
        }

        let diff = cache.find_diff(&exclusions);

        let paths_failed_to_add =
            super::apply_diff_and_print::<TimeMachine>(&diff, dry_run, details);

        for path in paths_failed_to_add {
            exclusions.remove(&path);
        }

        if !dry_run {
            cache.reset(exclusions);
        }
    }

    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use crate::cache::Cache;

    #[test]
    fn test_command() {
        let temp_dir = crate::commands::tests::create_repository(Some("test_run_command"));
        let mut cache = Cache::open_in_memory().unwrap();
        let config = crate::commands::tests::create_config(temp_dir.path());
        let dry_run = false;
        super::execute(&config, &mut cache, dry_run, false).unwrap();
        let temp_dir_path = temp_dir.path().canonicalize().unwrap();
        let a_file_path = temp_dir_path.join("a");
        let b_file_path = temp_dir_path.join("b");
        let c_file_path = temp_dir_path.join("c");
        let paths = cache.paths();
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
    fn test_dry_run() {
        let temp_dir =
            crate::commands::tests::create_repository(Some("run_command_test_command_dry_run"));
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
        assert_eq!(0, cache.paths().len());
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
