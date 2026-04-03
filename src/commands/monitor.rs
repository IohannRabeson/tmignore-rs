use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    thread::JoinHandle,
};

use crossbeam_channel::{Receiver, Sender, select};
use log::{debug, warn};
use notify::{FsEventWatcher, Watcher};

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
/// Each 60 seconds by default the changes found in the file system are applied to the list of excluded files.
/// The configuration file is watched, if it is modified it will be reloaded and a complete scan will start.
/// If a .gitignore file is modified then a scan of the repository will be scheduled.
pub fn execute(
    config_file_path: impl AsRef<Path>,
    cache: &mut Cache,
    dry_run: bool,
    details: bool,
    logger: &mut Logger,
    monitor: &mut impl MonitorTrait,
) -> Result<(), anyhow::Error> {
    let config_file_path = config_file_path.as_ref().canonicalize()?.clone();
    let mut config = Config::load_or_create_file(&config_file_path)?;
    let mut whitelist = super::create_whitelist(&config.whitelist_patterns)?;

    super::run::execute(&config, cache, dry_run, details, logger)?;

    monitor.set_watched_directories(&config.search_directories);
    logger.log("Monitor started");

    loop {
        if let Some(event) = monitor.get_event() {
            match event {
                Event::ReloadConfiguration => match config.reload_file(&config_file_path) {
                    Ok(()) => {
                        whitelist = super::create_whitelist(&config.whitelist_patterns)?;
                        monitor.set_watched_directories(&config.search_directories);
                        logger.log("Configuration reloaded");
                        super::run::execute(&config, cache, dry_run, details, logger)?;
                    }
                    Err(error) => {
                        warn!(
                            "Failed to reload configuration '{}': {}",
                            config_file_path.display(),
                            error
                        );
                        warn!("Due to an error the configuration stay unchanged");
                    }
                },
                Event::ScanRepositories(repositories_to_scan) => {
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
                            exclusions.remove(&path);
                        }

                        if !dry_run {
                            cache.remove_paths_in_directory(repository_to_scan);
                            cache.add_paths(exclusions.into_iter());
                        }
                    }
                }
                Event::Shutdown => {
                    break;
                }
            }
        }
    }
    monitor.shutdown();
    logger.log("Monitor stopped");
    Ok(())
}

/// Search the repositories related to an event.
/// The repositories listed are in one of the search directories.
fn find_repositories(
    search_directories: &BTreeSet<PathBuf>,
    event: &notify::Event,
) -> BTreeSet<PathBuf> {
    let mut results = BTreeSet::new();

    for path in &event.paths {
        if let Some(repository_path) = git::find_parent_repository(path)
            && search_directories
                .iter()
                .any(|search_directory| repository_path.starts_with(search_directory))
        {
            results.insert(repository_path);
        }
    }

    results
}

#[derive(PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Event {
    /// Request to reload the configuration
    ReloadConfiguration,
    /// Request to scan some repositories
    ScanRepositories(BTreeSet<PathBuf>),
    /// Shutdown
    ///
    /// Keep this constant the last one to ensure this event will be the last to
    /// be processed.
    Shutdown,
}

pub trait MonitorTrait {
    /// Set the watched directories.
    /// The previous directories are cleared.
    fn set_watched_directories(&mut self, directory_paths: &BTreeSet<PathBuf>)
    -> Vec<MonitorError>;
    fn get_event(&mut self) -> Option<Event>;
    fn shutdown(&mut self);
}

pub struct Monitor {
    watcher: FsEventWatcher,
    watched_paths: BTreeSet<PathBuf>,
    configuration_file_path: PathBuf,
    global_gitignore: Option<PathBuf>,
    event_receiver: Receiver<notify::Result<notify::Event>>,
    signal_thread_handle: Option<JoinHandle<()>>,
    signal_receiver: Receiver<()>,
}

#[derive(thiserror::Error, Debug)]
pub enum MonitorError {
    #[error(transparent)]
    Notify(#[from] notify::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl Monitor {
    pub fn new(
        configuration_file_path: impl AsRef<Path>,
        global_gitignore: Option<PathBuf>,
    ) -> Result<Self, MonitorError> {
        let configuration_file_path = configuration_file_path.as_ref().to_path_buf();
        let (event_sender, event_receiver) =
            crossbeam_channel::bounded::<notify::Result<notify::Event>>(256);
        let mut watcher = notify::recommended_watcher(EventHandler::new(event_sender))?;
        watcher.watch(
            &configuration_file_path,
            notify::RecursiveMode::NonRecursive,
        )?;
        if let Some(global_gitignore) = global_gitignore.as_ref() {
            watcher.watch(global_gitignore, notify::RecursiveMode::NonRecursive)?;
            debug!(
                "Watch global gitignore file '{}'",
                global_gitignore.display()
            );
        }

        let mut signals = signal_hook::iterator::Signals::new([
            signal_hook::consts::SIGTERM,
            signal_hook::consts::SIGINT,
        ])
        .unwrap();
        let (signal_sender, signal_receiver) = crossbeam_channel::bounded(1);
        let signal_thread_handle = std::thread::spawn(move || {
            debug!("Signals thread starts");
            if (&mut signals).into_iter().next().is_some() {
                let _ = signal_sender.send(());
            }
            debug!("Signals thread shutdowns");
        });

        Ok(Self {
            watcher,
            configuration_file_path,
            global_gitignore,
            watched_paths: BTreeSet::new(),
            event_receiver,
            signal_thread_handle: Some(signal_thread_handle),
            signal_receiver,
        })
    }

    fn accept_event(event: &notify::Event) -> bool {
        match &event.kind {
            notify::EventKind::Create(_)
            | notify::EventKind::Remove(_)
            | notify::EventKind::Modify(notify::event::ModifyKind::Name(_)) => (),
            notify::EventKind::Modify(notify::event::ModifyKind::Data(_)) => {
                // If there is no path that ends with ".gitignore" then reject the event
                if !event.paths.iter().any(|path| path.ends_with(".gitignore")) {
                    return false;
                }
            }
            _ => return false,
        }
        true
    }
}

impl MonitorTrait for Monitor {
    fn set_watched_directories(
        &mut self,
        directory_paths: &BTreeSet<PathBuf>,
    ) -> Vec<MonitorError> {
        let mut errors = vec![];
        let mut paths = self.watcher.paths_mut();
        for path_to_remove in &self.watched_paths {
            let _ = paths.remove(path_to_remove);
        }
        self.watched_paths.clear();
        for path_to_add in directory_paths {
            match paths.add(path_to_add, notify::RecursiveMode::Recursive) {
                Ok(()) => {
                    self.watched_paths.insert(path_to_add.clone());
                }
                Err(error) => {
                    errors.push(error.into());
                }
            }
        }
        let _ = paths.commit();
        errors
    }

    fn get_event(&mut self) -> Option<Event> {
        select! {
            recv(self.signal_receiver) -> _signal => return Some(Event::Shutdown),
            recv(self.event_receiver) -> event => {
                if let Ok(Ok(event)) = event {
                    if event.paths.contains(&self.configuration_file_path)
                        || self
                            .global_gitignore
                            .as_ref()
                            .is_some_and(|global_gitignore| event.paths.contains(global_gitignore))
                    {
                        return Some(Event::ReloadConfiguration);
                    }

                    if Self::accept_event(&event) {
                        let repositories_paths = find_repositories(&self.watched_paths, &event);
                        if !repositories_paths.is_empty() {
                            return Some(Event::ScanRepositories(repositories_paths));
                        }
                    }
                }
            }
        }

        None
    }

    fn shutdown(&mut self) {
        if let Some(handle) = self.signal_thread_handle.take() {
            handle.join().unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
        thread::{self, JoinHandle},
        time::{Duration, Instant},
    };

    use crossbeam_channel::{Receiver, Sender};
    use rstest::rstest;
    use serial_test::serial;
    use temp_dir_builder::TempDirectoryBuilder;

    use crate::{
        Logger,
        cache::Cache,
        commands::{
            monitor::{self, Event, Monitor, MonitorTrait},
            tests::init_git_repository,
        },
    };

    struct MockMonitor {
        event_receiver: Receiver<Event>,
        watched_paths: BTreeSet<PathBuf>,
    }

    impl MockMonitor {
        pub fn new() -> (Self, Sender<Event>) {
            let (event_sender, event_receiver) = crossbeam_channel::bounded(16);
            (
                Self {
                    event_receiver,
                    watched_paths: BTreeSet::new(),
                },
                event_sender,
            )
        }
    }

    impl MonitorTrait for MockMonitor {
        fn set_watched_directories(
            &mut self,
            directory_paths: &BTreeSet<PathBuf>,
        ) -> Vec<monitor::MonitorError> {
            self.watched_paths = directory_paths.clone();
            vec![]
        }

        fn get_event(&mut self) -> Option<Event> {
            self.event_receiver.recv().ok()
        }

        fn shutdown(&mut self) {}
    }

    #[test]
    fn test_monitor_basic() {
        let temp_dir = crate::commands::tests::create_repository(None::<PathBuf>);
        let temp_dir_path = temp_dir.path().canonicalize().unwrap();
        let file_a_path = temp_dir_path.join("a");
        let file_b_path = temp_dir_path.join("b");
        let (mut monitor, event_sender) = MockMonitor::new();
        let config = crate::commands::tests::create_config(temp_dir.path());
        let config_file_path = temp_dir.path().join("config.json");
        config.save_to_file(&config_file_path).unwrap();

        let handle = thread::spawn(move || {
            let mut cache = Cache::open_in_memory().unwrap();
            let dry_run = false;
            let mut logger = Logger::new(dry_run);
            super::execute(
                config_file_path,
                &mut cache,
                dry_run,
                true,
                &mut logger,
                &mut monitor,
            )
            .unwrap();

            cache
        });

        event_sender.send(Event::Shutdown).unwrap();
        let cache = handle.join().unwrap();
        let paths = cache.paths();
        assert_eq!(2, paths.len());
        assert!(paths.contains(&file_a_path));
        assert!(paths.contains(&file_b_path));
    }

    #[test]
    fn test_monitor_update_config() {
        let temp_dir = crate::commands::tests::create_repository(None::<PathBuf>);
        let empty_directory = temp_dir.path().join("empty");
        let temp_dir_path = temp_dir.path().canonicalize().unwrap();
        let file_a_path = temp_dir_path.join("a");
        let file_b_path = temp_dir_path.join("b");
        std::fs::create_dir_all(&empty_directory).unwrap();
        let mut config = crate::commands::tests::create_config(&empty_directory);
        config.monitor_interval = Duration::ZERO;
        let config_file_path = temp_dir.path().join("config.json");
        config.save_to_file(&config_file_path).unwrap();
        let config_file_path_thread = config_file_path.clone();
        let (mut monitor, event_sender) = MockMonitor::new();
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
                &mut monitor,
            )
            .unwrap();

            cache
        });
        config.search_directories.clear();
        config
            .search_directories
            .insert(temp_dir.path().to_path_buf());
        config.save_to_file(&config_file_path).unwrap();
        event_sender.send(Event::ReloadConfiguration).unwrap();
        event_sender.send(Event::Shutdown).unwrap();
        let cache = handle.join().unwrap();
        let paths = cache.paths();
        assert_eq!(2, paths.len());
        assert!(paths.contains(&file_a_path));
        assert!(paths.contains(&file_b_path));
    }

    #[test]
    fn test_monitor_update_config_invalid() {
        let temp_dir = crate::commands::tests::create_repository(None::<PathBuf>);
        let empty_directory = temp_dir.path().join("empty");
        std::fs::create_dir_all(&empty_directory).unwrap();
        let mut config = crate::commands::tests::create_config(&empty_directory);
        config.monitor_interval = Duration::ZERO;
        let config_file_path = temp_dir.path().join("config.json");
        config.save_to_file(&config_file_path).unwrap();
        let config_file_path_thread = config_file_path.clone();
        let (mut monitor, event_sender) = MockMonitor::new();
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
                &mut monitor,
            )
            .unwrap();

            cache
        });
        // Sleep to ensure the file is not written again before the scan finishes because
        // at the scan phase, an invalid configuration will stop the program.
        // Here we want to test the monitoring will not stop even if the configuration become invalid
        // after the initial scan.
        thread::sleep(Duration::from_millis(200));
        std::fs::write(&config_file_path, "invalid json").unwrap();
        // Sleep to ensure the file is fully written when the ReloadConfiguration event
        // is processed.
        thread::sleep(Duration::from_millis(200));
        event_sender.send(Event::ReloadConfiguration).unwrap();
        event_sender.send(Event::Shutdown).unwrap();
        let _cache = handle.join().unwrap();
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
        let temp_dir = crate::commands::tests::create_repository(None::<PathBuf>);
        let temp_dir_path = temp_dir.path().canonicalize().unwrap();
        let file_a_path = temp_dir_path.join("a");
        let file_b_path = temp_dir_path.join("b");
        set_permission(file_b_path, 0).unwrap();
        let config = crate::commands::tests::create_config(temp_dir.path());
        let config_file_path = temp_dir.path().join("config.json");
        config.save_to_file(&config_file_path).unwrap();
        let (mut monitor, event_sender) = MockMonitor::new();
        let handle = thread::spawn(move || {
            let mut cache = Cache::open_in_memory().unwrap();
            let dry_run = false;
            let mut logger = Logger::new(dry_run);
            super::execute(
                config_file_path,
                &mut cache,
                dry_run,
                true,
                &mut logger,
                &mut monitor,
            )
            .unwrap();

            cache
        });
        event_sender.send(Event::ReloadConfiguration).unwrap();
        event_sender
            .send(Event::ScanRepositories(BTreeSet::from([
                temp_dir_path.clone()
            ])))
            .unwrap();
        event_sender.send(Event::Shutdown).unwrap();
        let cache = handle.join().unwrap();
        let paths = cache.paths();
        assert_eq!(1, paths.len());
        assert_eq!(paths[0], file_a_path);
    }

    #[test]
    fn test_monitor_update_config_error() {
        let temp_dir = crate::commands::tests::create_repository(None::<PathBuf>);
        let empty_directory = temp_dir.path().join("empty");
        let temp_dir_path = temp_dir.path().canonicalize().unwrap();
        let file_a_path = temp_dir_path.join("a");
        let file_b_path = temp_dir_path.join("b");
        set_permission(file_b_path, 0).unwrap();
        std::fs::create_dir_all(&empty_directory).unwrap();
        let mut config = crate::commands::tests::create_config(&empty_directory);
        config.monitor_interval = Duration::from_secs(1);
        let config_file_path = temp_dir.path().join("config.json");
        config.save_to_file(&config_file_path).unwrap();
        let config_file_path_thread = config_file_path.clone();
        let (mut monitor, event_sender) = MockMonitor::new();
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
                &mut monitor,
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
        event_sender.send(Event::ReloadConfiguration).unwrap();
        thread::sleep(Duration::from_millis(1200));
        event_sender.send(Event::Shutdown).unwrap();

        let cache = handle.join().unwrap();
        let paths = cache.paths();
        assert_eq!(1, paths.len());
        assert_eq!(file_a_path, paths[0]);
    }

    #[test]
    #[serial]
    fn test_monitor_removed_file() {
        let temp_dir = crate::commands::tests::create_repository(None::<PathBuf>);
        let temp_dir_path = temp_dir.path().canonicalize().unwrap();
        let file_a_path = temp_dir_path.join("a");
        let file_b_path = temp_dir_path.join("b");
        let config = crate::commands::tests::create_config(&temp_dir_path);
        let config_file_path = temp_dir_path.join("config.json");
        config.save_to_file(&config_file_path).unwrap();
        let mut monitor = Monitor::new(&config_file_path, None).unwrap();
        let handle = thread::spawn(move || {
            let mut cache = Cache::open_in_memory().unwrap();
            let dry_run = false;
            let mut logger = Logger::new(dry_run);
            super::execute(
                config_file_path,
                &mut cache,
                dry_run,
                true,
                &mut logger,
                &mut monitor,
            )
            .unwrap();

            cache
        });
        thread::sleep(Duration::from_millis(200));
        std::fs::remove_file(file_b_path).unwrap();
        std::thread::sleep(Duration::from_millis(1000));
        unsafe {
            libc::kill(libc::getpid(), signal_hook::consts::SIGINT);
        }
        let cache = handle.join().unwrap();
        let paths = cache.paths();
        assert_eq!(1, paths.len());
        assert_eq!(file_a_path, paths[0]);
    }

    #[test]
    #[serial]
    fn test_monitor_renamed_file() {
        let temp_dir = crate::commands::tests::create_repository(None::<PathBuf>);
        let temp_dir_path = temp_dir.path().canonicalize().unwrap();
        let file_a_path = temp_dir_path.join("a");
        let file_b_path = temp_dir_path.join("b");
        let file_d_path = temp_dir_path.join("d");
        let mut config = crate::commands::tests::create_config(&temp_dir_path);
        config.monitor_interval = Duration::from_secs(1);
        let config_file_path = temp_dir_path.join("config.json");
        config.save_to_file(&config_file_path).unwrap();
        let mut monitor = Monitor::new(&config_file_path, None).unwrap();
        let handle = thread::spawn(move || {
            let mut cache = Cache::open_in_memory().unwrap();
            let dry_run = false;
            let mut logger = Logger::new(dry_run);
            super::execute(
                config_file_path,
                &mut cache,
                dry_run,
                true,
                &mut logger,
                &mut monitor,
            )
            .unwrap();

            cache
        });
        thread::sleep(Duration::from_millis(200));
        std::fs::rename(file_b_path, file_d_path).unwrap();
        thread::sleep(Duration::from_millis(1000));
        unsafe {
            libc::kill(libc::getpid(), signal_hook::consts::SIGINT);
        }
        let cache = handle.join().unwrap();
        let paths = cache.paths();
        assert_eq!(1, paths.len());
        assert_eq!(file_a_path, paths[0]);
    }

    #[test]
    #[serial]
    fn test_monitor_add_a_repository() {
        let temp_dir = TempDirectoryBuilder::default().build().unwrap();
        let root_folder_path = temp_dir.path().canonicalize().unwrap();
        let mut config = crate::commands::tests::create_config(&root_folder_path);
        let config_file_path = root_folder_path.join("config.json");
        config.monitor_interval = Duration::from_millis(100);
        config.save_to_file(&config_file_path).unwrap();
        let mut monitor = Monitor::new(&config_file_path, None).unwrap();
        let handle = thread::spawn(move || {
            let mut cache = Cache::open_in_memory().unwrap();
            let dry_run = false;
            let mut logger = Logger::new(dry_run);
            super::execute(
                config_file_path,
                &mut cache,
                dry_run,
                true,
                &mut logger,
                &mut monitor,
            )
            .unwrap();

            cache
        });
        // Ensure the monitor in the other thread is properly initialized
        // before starting to create events.
        thread::sleep(Duration::from_millis(400));
        let new_repository_path = root_folder_path.join("new repository");
        std::fs::create_dir_all(&new_repository_path).unwrap();
        init_git_repository(&new_repository_path);
        let gitignore_file_path = new_repository_path.join(".gitignore");
        let file_a_path = new_repository_path.join("a");
        let file_b_path = new_repository_path.join("b");
        std::fs::File::create(&file_a_path).unwrap();
        std::fs::write(gitignore_file_path, "a\nb\n").unwrap();
        std::fs::File::create(&file_b_path).unwrap();
        thread::sleep(Duration::from_millis(1000));
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

    /// Wait for a specific event
    ///
    /// The other events will be ignored. The reason for this behavior is I can't control what is happening
    /// on the filesystem and it's possible events arrive late.
    /// An example of issue I had sometimes is instead of the Shutdown event I was waiting I was getting a ScanRepositories
    /// instead because it was detecting the creation of the new folder for the repository a little bit late.
    fn wait_for_event(monitor: &mut Monitor, timeout: Duration, expected_event: &Event) -> bool {
        let started = Instant::now();
        while Instant::now() - started < timeout {
            if let Some(event) = monitor.get_event()
                && &event == expected_event
            {
                return true;
            }
        }
        false
    }

    fn spawn_monitor_thread_and_wait_for_event(
        config_file_path: PathBuf,
        global_gitignore: Option<PathBuf>,
        repository_path: PathBuf,
        event: Event,
    ) -> JoinHandle<bool> {
        let mut monitor = Monitor::new(&config_file_path, global_gitignore).unwrap();
        monitor.set_watched_directories(&BTreeSet::from([repository_path]));
        std::thread::spawn(move || wait_for_event(&mut monitor, Duration::from_secs(20), &event))
    }

    #[rstest]
    #[case(notify::Event::default().set_kind(notify::EventKind::Create(notify::event::CreateKind::File)), true)]
    #[case(notify::Event::default().set_kind(notify::EventKind::Remove(notify::event::RemoveKind::File)), true)]
    #[case(notify::Event::default().set_kind(notify::EventKind::Modify(notify::event::ModifyKind::Data(notify::event::DataChange::Content))).add_path(".gitignore".into()), true)]
    #[case(notify::Event::default().set_kind(notify::EventKind::Modify(notify::event::ModifyKind::Data(notify::event::DataChange::Content))).add_path("yop".into()), false)]
    #[case(notify::Event::default().set_kind(notify::EventKind::Modify(notify::event::ModifyKind::Name(notify::event::RenameMode::From))), true)]
    #[case(notify::Event::default().set_kind(notify::EventKind::Access(notify::event::AccessKind::Read)), false)]
    #[case(notify::Event::default().set_kind(notify::EventKind::Other), false)]
    fn test_accept_event(#[case] event: notify::Event, #[case] accepted: bool) {
        let result = Monitor::accept_event(&event);

        assert_eq!(accepted, result);
    }

    #[test]
    #[serial]
    fn test_monitor_add_file() {
        let temp_dir = TempDirectoryBuilder::default()
            .add_directory("repository")
            .add_directory("repository/.git")
            .add_empty_file("config.json")
            .build()
            .unwrap();
        let temp_dir_path = temp_dir.path().canonicalize().unwrap();
        let repository_path = temp_dir_path.join("repository");
        let config_file_path = temp_dir_path.join("config.json");
        std::thread::sleep(Duration::from_millis(500));
        let handle = spawn_monitor_thread_and_wait_for_event(
            config_file_path,
            None,
            repository_path.clone(),
            Event::ScanRepositories(BTreeSet::from([repository_path.clone()])),
        );
        std::fs::File::create(repository_path.join("new_file")).unwrap();
        assert!(handle.join().unwrap());
    }

    #[test]
    #[serial]
    fn test_monitor_reload_config() {
        let temp_dir = TempDirectoryBuilder::default()
            .add_directory("repository")
            .add_directory("repository/.git")
            .add_empty_file("config.json")
            .build()
            .unwrap();
        let temp_dir_path = temp_dir.path().canonicalize().unwrap();
        let repository_path = temp_dir_path.join("repository");
        let config_file_path = temp_dir_path.join("config.json");
        std::thread::sleep(Duration::from_millis(500));
        let handle = spawn_monitor_thread_and_wait_for_event(
            config_file_path.clone(),
            None,
            repository_path.clone(),
            Event::ReloadConfiguration,
        );
        std::fs::write(&config_file_path, "Hey").unwrap();
        assert!(handle.join().unwrap());
    }

    #[test]
    #[serial]
    fn test_monitor_shutdown() {
        let temp_dir = TempDirectoryBuilder::default()
            .add_directory("repository")
            .add_directory("repository/.git")
            .add_empty_file("config.json")
            .build()
            .unwrap();
        let temp_dir_path = temp_dir.path().canonicalize().unwrap();
        let repository_path = temp_dir_path.join("repository");
        let config_file_path = temp_dir_path.join("config.json");
        let handle = spawn_monitor_thread_and_wait_for_event(
            config_file_path,
            None,
            repository_path.clone(),
            Event::Shutdown,
        );
        unsafe {
            libc::kill(libc::getpid(), signal_hook::consts::SIGINT);
        }
        assert!(handle.join().unwrap());
    }

    #[test]
    #[serial]
    fn test_set_watched_directories_error() {
        let temp_dir = TempDirectoryBuilder::default()
            .add_directory("repository")
            .add_directory("repository/.git")
            .add_empty_file("config.json")
            .build()
            .unwrap();
        let temp_dir_path = temp_dir.path().canonicalize().unwrap();
        let config_file_path = temp_dir_path.join("config.json");
        let mut monitor = Monitor::new(config_file_path, None).unwrap();
        let errors = monitor
            .set_watched_directories(&BTreeSet::from([temp_dir_path.join("does not exist")]));
        assert_eq!(1, errors.len());
    }

    #[test]
    #[serial]
    fn test_global_gitignore() {
        let dir_path = PathBuf::from("test_global_gitignore");
        if dir_path.is_dir() {
            std::fs::remove_dir_all(&dir_path).unwrap();
        }
        let temp_dir = TempDirectoryBuilder::default()
            .root_folder(dir_path)
            .add_empty_file("global_gitignore")
            .add_directory("repository")
            .add_directory("repository/.git")
            .add_empty_file("config.json")
            .build()
            .unwrap();
        let temp_dir_path = temp_dir.path().canonicalize().unwrap();
        let repository_path = temp_dir_path.join("repository");
        crate::commands::tests::init_git_repository(&repository_path);
        let global_gitignore_path = temp_dir_path.join("global_gitignore");
        let config_file_path = temp_dir_path.join("config.json");
        std::thread::sleep(Duration::from_millis(500));
        let handle = spawn_monitor_thread_and_wait_for_event(
            config_file_path,
            Some(global_gitignore_path.clone()),
            repository_path.clone(),
            Event::ReloadConfiguration,
        );
        std::fs::write(global_gitignore_path, "yo").unwrap();
        assert!(handle.join().unwrap());
    }
}
