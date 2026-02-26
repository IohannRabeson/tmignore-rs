use std::{
        collections::BTreeSet,
        error::Error,
        path::{Path, PathBuf},
        sync::{Arc, atomic::AtomicBool},
        time::{Duration, Instant},
    };

    use crossbeam_channel::Sender;
    use notify::Watcher;

    use crate::{
        Logger, cache::Cache, config::Config, git,
    };

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
        let config_file_path = config_file_path.as_ref().to_path_buf();
        let mut config = Config::load_or_create_file(&config_file_path)?;
        let signal = Arc::new(AtomicBool::new(false));
        signal_hook::flag::register(signal_hook::consts::SIGTERM, signal.clone())?;
        signal_hook::flag::register(signal_hook::consts::SIGINT, signal.clone())?;
        let (fs_event_sender, fs_event_receiver) =
            crossbeam_channel::bounded::<notify::Result<notify::Event>>(256);
        let _watcher = create_watcher(fs_event_sender, config.search_directories.iter());
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
                            notify::EventKind::Modify(notify::event::ModifyKind::Data(_))
                        ) && event.paths.contains(&config_file_path)
                        {
                            config.reload_file(&config_file_path)?;
                            whitelist = super::create_whitelist(&config.whitelist_patterns)?;
                            println!("Configuration reloaded");
                            super::run::execute(&config, cache, dry_run, details, logger)?;
                        }

                        if accept_event(&config, &event) {
                            let repositories_paths = find_repositories(&event);

                            for path in repositories_paths {
                                repositories_to_scan.insert(path);
                            }
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
            if !repositories_to_scan.is_empty() && elapsed >= run_interval {
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
                    let paths_failed_to_add = super::apply_diff_and_print(&diff, dry_run, details, logger);

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

    fn find_repositories(event: &notify::Event) -> BTreeSet<PathBuf> {
        let mut results = BTreeSet::new();

        for path in &event.paths {
            if let Some(repository_path) = git::find_parent_repository(path) {
                results.insert(repository_path);
            }
        }

        results
    }

    fn accept_event(config: &Config, event: &notify::Event) -> bool {
        match &event.kind {
            notify::EventKind::Create(_) => (),
            notify::EventKind::Modify(notify::event::ModifyKind::Name(_)) => (),
            notify::EventKind::Modify(notify::event::ModifyKind::Data(_)) => {
                if !event.paths.iter().any(|path| path.ends_with(".gitignore")) {
                    return false;
                }
            }
            notify::EventKind::Remove(_) => (),
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
    ) -> notify::Result<notify::RecommendedWatcher> {
        let mut watcher = notify::recommended_watcher(EventHandler::new(sender))?;
        let mut watcher_paths = watcher.paths_mut();

        for directory_path in search_directories {
            watcher_paths.add(directory_path, notify::RecursiveMode::Recursive)?;
        }

        watcher_paths.commit()?;

        Ok(watcher)
    }