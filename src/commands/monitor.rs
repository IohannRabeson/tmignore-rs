use std::{
    collections::BTreeSet,
    error::Error,
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};

use crossbeam_channel::Sender;
use notify::Watcher;

use crate::{Logger, cache::Cache, commands::TimeMachine, config::Config, git};

struct EventHandler {
    sender: Sender<notify::Result<notify::Event>>,
}

impl EventHandler {
    fn new(sender: Sender<notify::Result<notify::Event>>) -> Self {
        Self { sender }
    }
}

impl notify::EventHandler for EventHandler {
    fn handle_event(&mut self, event: notify::Result<notify::Event>) {
        let _ = self.sender.send(event);
    }
}

/// This command monitors a set of directories for changes and keeps up to date the
/// list of paths to exclude from Time Machine backups.
/// It works by watching the search directories specified by the configuration file.
/// Each 5 seconds by default the changes found in the file system are applied to the list of excluded files.
/// The configuration file is watched, if it is modified it will be reloaded and a complete scan will start.
/// If a .gitignore file is modified then a scan of the repository will be scheduled.
pub fn execute(
    config_file_path: impl AsRef<Path>,
    cache: &mut Cache,
    dry_run: bool,
    details: bool,
    logger: &mut Logger,
) -> Result<(), Box<dyn Error>> {
    let config_file_path = config_file_path.as_ref().canonicalize()?.to_path_buf();
    let mut config = Config::load_or_create_file(&config_file_path)?;
    let signal = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, signal.clone())?;
    signal_hook::flag::register(signal_hook::consts::SIGINT, signal.clone())?;
    let (fs_event_sender, fs_event_receiver) =
        crossbeam_channel::bounded::<notify::Result<notify::Event>>(256);
    let mut _watcher = create_watcher(
        fs_event_sender.clone(),
        config.search_directories.iter(),
        &config_file_path,
    );
    let mut elapsed = Duration::ZERO;
    let mut now = Instant::now();
    let mut repositories_to_scan = BTreeSet::new();
    let mut whitelist = super::create_whitelist(&config.whitelist_patterns)?;

    super::run::execute(&config, cache, dry_run, details, logger)?;

    logger.log("Monitor started");
    while !signal.load(std::sync::atomic::Ordering::Relaxed) {
        match fs_event_receiver.recv_timeout(Duration::from_millis(250)) {
            Ok(event) => {
                if let Ok(event) = event {
                    if matches!(
                        event.kind,
                        notify::EventKind::Create(notify::event::CreateKind::File)
                    ) && event.paths.contains(&config_file_path)
                    {
                        config.reload_file(&config_file_path)?;
                        _watcher = create_watcher(
                            fs_event_sender.clone(),
                            config.search_directories.iter(),
                            &config_file_path,
                        );
                        whitelist = super::create_whitelist(&config.whitelist_patterns)?;
                        println!("Configuration reloaded");
                        super::run::execute(&config, cache, dry_run, details, logger)?;
                    }

                    if accept_event(&config, &event) {
                        let repositories_paths = find_repositories(&config.search_directories, &event);

                        for path in repositories_paths {
                            repositories_to_scan.insert(path);
                        }
                    } else {
                        println!("Event rejected");
                    }
                }
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => (),
            Err(error) => return Err(Box::new(error)),
        }

        elapsed += Instant::now() - now;
        now = Instant::now();
        let run_interval = Duration::from_secs(
            config
                .monitor_interval_secs
                .unwrap_or(Config::DEFAULT_MONITOR_INTERVAL_SECS),
        );
        if !repositories_to_scan.is_empty()
            && (elapsed >= run_interval || signal.load(std::sync::atomic::Ordering::Relaxed))
        {
            for repository_to_scan in &repositories_to_scan {
                logger.log(format!(
                    "Scanning repository '{}'",
                    repository_to_scan.display()
                ));
                let mut exclusions = BTreeSet::new();
                super::find_paths_to_exclude_from_backup(
                    repository_to_scan,
                    &whitelist,
                    &mut exclusions,
                )?;
                let diff = cache.find_diff_in_directory(&exclusions, repository_to_scan);
                let paths_failed_to_add = super::apply_diff_and_print::<TimeMachine>(
                    &diff, dry_run, details, logger,
                );

                for path in paths_failed_to_add {
                    exclusions.remove(path);
                }

                if !dry_run {
                    cache.remove_paths_in_directory(repository_to_scan);
                    cache.add_paths(exclusions.into_iter());
                }
            }
            repositories_to_scan.clear();
            elapsed = Duration::ZERO;
        }
    }
    logger.log("Monitor stopped");
    Ok(())
}

/// Search the repositories related to an event.
/// The repositories listed are in one of the search directories.
fn find_repositories(search_directories: &BTreeSet<PathBuf>, event: &notify::Event) -> BTreeSet<PathBuf> {
    let mut results = BTreeSet::new();

    for path in &event.paths {
        if let Some(repository_path) = git::find_parent_repository(path) {
            if search_directories.iter().any(|search_directory|{
                repository_path.starts_with(search_directory)
            }) {
                results.insert(repository_path);
            }
        }
    }

    results
}

fn accept_event(config: &Config, event: &notify::Event) -> bool {
    match &event.kind {
        notify::EventKind::Create(_) => (),
        notify::EventKind::Modify(notify::event::ModifyKind::Name(_)) => (),
        notify::EventKind::Remove(_) => (),
        notify::EventKind::Modify(notify::event::ModifyKind::Data(_)) => {
            // If there is no path that ends with ".gitignore" then reject the event
            if !event.paths.iter().any(|path| path.ends_with(".gitignore")) {
                return false;
            }
        }
        _ => return false,
    }

    config.ignored_directories.iter().all(|ignored_directory| {
        for path in &event.paths {
            if path.starts_with(ignored_directory) {
                return false;
            }
        }

        true
    })
}

fn create_watcher<'a>(
    sender: Sender<notify::Result<notify::Event>>,
    search_directories: impl Iterator<Item = &'a PathBuf>,
    configuration_file_path: impl AsRef<Path>,
) -> notify::Result<notify::RecommendedWatcher> {
    let mut watcher = notify::recommended_watcher(EventHandler::new(sender))?;
    let mut watcher_paths = watcher.paths_mut();

    for directory_path in search_directories {
        watcher_paths.add(directory_path, notify::RecursiveMode::Recursive)?;
    }
    watcher_paths.add(
        configuration_file_path.as_ref(),
        notify::RecursiveMode::NonRecursive,
    )?;

    watcher_paths.commit()?;

    Ok(watcher)
}

#[cfg(test)]
mod tests {
    use std::{os::unix::fs::PermissionsExt, path::{Path, PathBuf}, thread, time::Duration};

    use temp_dir_builder::TempDirectoryBuilder;

    use crate::{Logger, cache::Cache, commands::{monitor::accept_event, tests::init_git_repository}, config::Config};

    #[test]
    fn test_monitor_basic() {
        let temp_dir = crate::commands::tests::create_repository("test_monitor_basic");
        let temp_dir_path = temp_dir.path().canonicalize().unwrap();
        let file_a_path = temp_dir_path.join("a");
        let file_b_path = temp_dir_path.join("b");

        let config = crate::commands::tests::create_config(temp_dir.path());
        let config_file_path = temp_dir.path().join("config.json");
        config.save_to_file(&config_file_path).unwrap();

        let handle = thread::spawn(move || {
            let mut cache = Cache::open_in_memory().unwrap();
            let dry_run = false;
            let mut logger = Logger::new(dry_run);
            super::execute(config_file_path, &mut cache, dry_run, true, &mut logger).unwrap();

            cache
        });
        thread::sleep(Duration::from_millis(200));

        unsafe {
            libc::kill(libc::getpid(), signal_hook::consts::SIGINT);
        }

        let cache = handle.join().unwrap();
        let paths = cache.paths();
        assert_eq!(2, paths.len());
        assert!(paths.contains(&file_a_path));
        assert!(paths.contains(&file_b_path));
    }

    #[test]
    fn test_monitor_update_config() {
        let temp_dir = crate::commands::tests::create_repository("test_monitor_update_config");
        let empty_directory = temp_dir.path().join("empty");
        let temp_dir_path = temp_dir.path().canonicalize().unwrap();
        let file_a_path = temp_dir_path.join("a");
        let file_b_path = temp_dir_path.join("b");
        std::fs::create_dir_all(&empty_directory).unwrap();
        let mut config = crate::commands::tests::create_config(&empty_directory);
        config.monitor_interval_secs = Some(1);
        let config_file_path = temp_dir.path().join("config.json");
        config.save_to_file(&config_file_path).unwrap();
        let config_file_path_thread = config_file_path.clone();
        let handle = thread::spawn(move || {
            let mut cache = Cache::open_in_memory().unwrap();
            let dry_run = false;
            let mut logger = Logger::new(dry_run);
            super::execute(
                config_file_path_thread,
                &mut cache,
                dry_run,
                true,
                &mut logger,
            )
            .unwrap();

            cache
        });
        thread::sleep(Duration::from_millis(200));
        config.search_directories.clear();
        config
            .search_directories
            .insert(temp_dir.path().to_path_buf());
        config.save_to_file(&config_file_path).unwrap();
        thread::sleep(Duration::from_millis(1200));
        unsafe {
            libc::kill(libc::getpid(), signal_hook::consts::SIGINT);
        }

        let cache = handle.join().unwrap();
        let paths = cache.paths();
        assert_eq!(2, paths.len());
        assert!(paths.contains(&file_a_path));
        assert!(paths.contains(&file_b_path));
    }

    fn set_permission(path: impl AsRef<Path>, mode: u32) -> Result<(), std::io::Error> {
        let path = path.as_ref();

        if !path.is_file() {
            return Ok(());
        }

        let f = std::fs::File::open(path)?;
        let metadata = f.metadata()?;
        let mut permissions = metadata.permissions();

        permissions.set_mode(mode);
        f.set_permissions(permissions)?;
        Ok(())
    }

    #[test]
    fn test_monitor_file_not_readable() {
        let temp_dir = crate::commands::tests::create_repository("test_monitor_file_not_readable");
        let temp_dir_path = temp_dir.path().canonicalize().unwrap();
        let file_a_path = temp_dir_path.join("a");
        let file_b_path = temp_dir_path.join("b");
        set_permission(file_b_path, 0).unwrap();
        let config = crate::commands::tests::create_config(temp_dir.path());
        let config_file_path = temp_dir.path().join("config.json");
        config.save_to_file(&config_file_path).unwrap();

        let handle = thread::spawn(move || {
            let mut cache = Cache::open_in_memory().unwrap();
            let dry_run = false;
            let mut logger = Logger::new(dry_run);
            super::execute(config_file_path, &mut cache, dry_run, true, &mut logger).unwrap();

            cache
        });
        thread::sleep(Duration::from_millis(200));

        unsafe {
            libc::kill(libc::getpid(), signal_hook::consts::SIGINT);
        }

        let cache = handle.join().unwrap();
        let paths = cache.paths();
        assert_eq!(1, paths.len());
        assert_eq!(paths[0], file_a_path);
    }

    #[test]
    fn test_monitor_update_config_error() {
        let temp_dir =
            crate::commands::tests::create_repository("test_monitor_update_config_error");
        let empty_directory = temp_dir.path().join("empty");
        let temp_dir_path = temp_dir.path().canonicalize().unwrap();
        let file_a_path = temp_dir_path.join("a");
        let file_b_path = temp_dir_path.join("b");
        set_permission(file_b_path, 0).unwrap();
        std::fs::create_dir_all(&empty_directory).unwrap();
        let mut config = crate::commands::tests::create_config(&empty_directory);
        config.monitor_interval_secs = Some(1);
        let config_file_path = temp_dir.path().join("config.json");
        config.save_to_file(&config_file_path).unwrap();
        let config_file_path_thread = config_file_path.clone();
        let handle = thread::spawn(move || {
            let mut cache = Cache::open_in_memory().unwrap();
            let dry_run = false;
            let mut logger = Logger::new(dry_run);
            super::execute(
                config_file_path_thread,
                &mut cache,
                dry_run,
                true,
                &mut logger,
            )
            .unwrap();

            cache
        });
        thread::sleep(Duration::from_millis(200));
        config.search_directories.clear();
        config
            .search_directories
            .insert(temp_dir.path().to_path_buf());
        config.save_to_file(&config_file_path).unwrap();
        thread::sleep(Duration::from_millis(1200));
        unsafe {
            libc::kill(libc::getpid(), signal_hook::consts::SIGINT);
        }

        let cache = handle.join().unwrap();
        let paths = cache.paths();
        assert_eq!(1, paths.len());
        assert_eq!(file_a_path, paths[0]);
    }

    #[test]
    fn test_monitor_removed_file() {
        let temp_dir = crate::commands::tests::create_repository("test_monitor_removed_file");
        let temp_dir_path = temp_dir.path().canonicalize().unwrap();
        let file_a_path = temp_dir_path.join("a");
        let file_b_path = temp_dir_path.join("b");

        let mut config = crate::commands::tests::create_config(&temp_dir_path);
        config.monitor_interval_secs = Some(0);
        let config_file_path = temp_dir_path.join("config.json");
        config.save_to_file(&config_file_path).unwrap();

        let handle = thread::spawn(move || {
            let mut cache = Cache::open_in_memory().unwrap();
            let dry_run = false;
            let mut logger = Logger::new(dry_run);
            super::execute(config_file_path, &mut cache, dry_run, true, &mut logger).unwrap();

            cache
        });
        thread::sleep(Duration::from_millis(200));
        std::fs::remove_file(file_b_path).unwrap();
        thread::sleep(Duration::from_millis(1200));
        unsafe {
            libc::kill(libc::getpid(), signal_hook::consts::SIGINT);
        }

        let cache = handle.join().unwrap();
        let paths = cache.paths();
        assert_eq!(1, paths.len());
        assert_eq!(file_a_path, paths[0]);
    }

    #[test]
    fn test_monitor_renamed_file() {
        let temp_dir = crate::commands::tests::create_repository("test_monitor_renamed_file");
        let temp_dir_path = temp_dir.path().canonicalize().unwrap();
        let file_a_path = temp_dir_path.join("a");
        let file_b_path = temp_dir_path.join("b");
        let file_c_path = temp_dir_path.join("c");
        let mut config = crate::commands::tests::create_config(&temp_dir_path);
        config.monitor_interval_secs = Some(1);
        let config_file_path = temp_dir_path.join("config.json");
        config.save_to_file(&config_file_path).unwrap();

        let handle = thread::spawn(move || {
            let mut cache = Cache::open_in_memory().unwrap();
            let dry_run = false;
            let mut logger = Logger::new(dry_run);
            super::execute(config_file_path, &mut cache, dry_run, true, &mut logger).unwrap();

            cache
        });
        thread::sleep(Duration::from_millis(200));
        std::fs::rename(file_b_path, file_c_path).unwrap();
        thread::sleep(Duration::from_millis(1200));
        unsafe {
            libc::kill(libc::getpid(), signal_hook::consts::SIGINT);
        }

        let cache = handle.join().unwrap();
        let paths = cache.paths();
        assert_eq!(1, paths.len());
        assert_eq!(file_a_path, paths[0]);
    }

    #[test]
    fn test_monitor_add_a_repository() {
        let root_folder_path = PathBuf::from("test_monitor_add_a_repository");
        if root_folder_path.exists() && root_folder_path.is_dir() {
            std::fs::remove_dir_all(&root_folder_path).unwrap();
        }
        let temp_dir = TempDirectoryBuilder::default().root_folder(root_folder_path).build().unwrap();
        let root_folder_path = temp_dir.path().canonicalize().unwrap();
        let mut config = crate::commands::tests::create_config(&root_folder_path);
        let config_file_path = root_folder_path.join("config.json");
        config.monitor_interval_secs = Some(0);
        config.save_to_file(&config_file_path).unwrap();

        let handle = thread::spawn(move || {
            let mut cache = Cache::open_in_memory().unwrap();
            let dry_run = false;
            let mut logger = Logger::new(dry_run);
            super::execute(config_file_path, &mut cache, dry_run, true, &mut logger).unwrap();

            cache
        });
        thread::sleep(Duration::from_millis(200));
        let new_repository_path = root_folder_path.join("new repository");
        std::fs::create_dir_all(&new_repository_path).unwrap();
        init_git_repository(&new_repository_path);
        let gitignore_file_path = new_repository_path.join(".gitignore");
        let file_a_path = new_repository_path.join("a");
        let file_b_path = new_repository_path.join("b");
        std::fs::File::create(&file_a_path).unwrap();
        std::fs::write(gitignore_file_path, "a\nb\n").unwrap();
        std::fs::File::create(&file_b_path).unwrap();
        thread::sleep(Duration::from_millis(200));
        unsafe {
            libc::kill(libc::getpid(), signal_hook::consts::SIGINT);
        }

        let cache = handle.join().unwrap();
        let paths = cache.paths();
        println!("{:?}", paths);
        assert_eq!(2, paths.len());
        assert!(paths.contains(&file_a_path));
        assert!(paths.contains(&file_b_path));
    }

    #[test]
    fn accept_event_ignored() {
        let mut config = Config::default();
        config.ignored_directories.insert("a".into());
        let event = notify::Event::new(notify::EventKind::Remove(notify::event::RemoveKind::File))
            .add_path("a".into());

        assert_eq!(false, accept_event(&config, &event));
    }
}
