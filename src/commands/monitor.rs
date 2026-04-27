use std::{
    collections::BTreeSet, ops::ControlFlow, path::{Path, PathBuf}, thread::JoinHandle, time::Duration
};

use anyhow::Context;
use crossbeam_channel::{Receiver, Sender};
use log::{debug, info, warn};
use regex::RegexSet;

use crate::{
    cache::Cache,
    commands::{
        TimeMachine,
        monitor::monitor_details::{DebouncerControl, MonitorControl, TimeMachineControl},
    },
    config::Config,
};

const EVENT_QUEUE_SIZE: usize = 128;

struct HandleEventContext<'a> {
    config: &'a mut Config,
    config_file_path: &'a Path,
    whitelist: &'a mut RegexSet,
    monitor: &'a mut Monitor,
    cache: &'a mut Cache,
    dry_run: bool,
    details: bool,
    is_timemachine_running: bool,
}

fn handle_event(
    context: &mut HandleEventContext<'_>,
    event: Event,
) -> Result<ControlFlow<()>, anyhow::Error> {
    match event {
        Event::ReloadConfiguration => match context.config.reload_file(context.config_file_path) {
            Ok(()) => {
                *context.whitelist = super::create_whitelist(&context.config.whitelist_patterns)?;
                context.monitor.set_watched_paths(&context.config.search_directories);
                context.monitor.set_debounce_duration(context.config.debounce_duration);
                debug!("Configuration reloaded");
                context.monitor.push_event(Event::InitialScan);
            }
            Err(error) => {
                warn!(
                    "Failed to reload configuration '{}': {}",
                    context.config_file_path.display(),
                    error
                );
                warn!("Due to an error the configuration stay unchanged");
            }
        },
        Event::InitialScan => {
            super::run::execute(context.config, context.cache, context.dry_run, context.details)?;
        }
        Event::ScanRepositories(repositories_to_scan) => {
            for repository_to_scan in &repositories_to_scan {
                debug!("Scanning repository '{}'", repository_to_scan.display());
                let mut exclusions = BTreeSet::new();
                super::find_paths_to_exclude_from_backup(
                    repository_to_scan,
                    context.whitelist,
                    &mut exclusions,
                )?;
                let diff = context.cache.find_diff_in_directory(&exclusions, repository_to_scan);
                let paths_failed_to_add =
                    super::apply_diff_and_print::<TimeMachine>(&diff, context.dry_run, context.details);
                for path in paths_failed_to_add {
                    exclusions.remove(&path);
                }
                if !context.dry_run {
                    context.cache.remove_paths_in_directory(repository_to_scan);
                    context.cache.add_paths(exclusions.into_iter());
                }
            }
        }
        Event::TimeMachineBackupFinished => {
            context.is_timemachine_running = false;
        }
        Event::Shutdown => return Ok(ControlFlow::Break(())),
    }
    Ok(ControlFlow::Continue(()))
}

pub fn execute(
    config_file_path: impl AsRef<Path>,
    global_gitignore_path: Option<&PathBuf>,
    cache: &mut Cache,
    dry_run: bool,
    details: bool,
) -> Result<(), anyhow::Error> {
    let config_file_path = std::path::absolute(&config_file_path).with_context(|| {
        format!(
            "Failed to get the absolute path for '{}'",
            config_file_path.as_ref().display()
        )
    })?;
    let mut config = Config::load_or_create_file(&config_file_path)?;
    let mut whitelist = super::create_whitelist(&config.whitelist_patterns)?;
    // Start the monitor before calling `super::run` to ensure the signals handlers are setup as soon as possible.
    let mut monitor = Monitor::new()?;

    monitor.push_event(Event::InitialScan);

    // Set configuration file after executing the `run` command to be sure to not catch the creation event
    // caused by `Config::load_or_create_file(&config_file_path)`.
    monitor.set_configuration_file(&config_file_path);
    if let Some(global_gitignore_path) = global_gitignore_path.as_ref() {
        monitor.set_global_gitignore(global_gitignore_path);
    }
    monitor.set_watched_paths(&config.search_directories);
    monitor.set_debounce_duration(config.debounce_duration);

    let mut context = HandleEventContext {
        config: &mut config,
        config_file_path: &config_file_path,
        whitelist: &mut whitelist,
        monitor: &mut monitor,
        cache,
        dry_run,
        details,
        is_timemachine_running: false,
    };
    let mut pending_events = BTreeSet::new();

    'outer: while let Some(event) = context.monitor.get_event() {
        if crate::timemachine::is_time_machine_running() && !context.is_timemachine_running {
            info!("Time Machine backup started");
            context.monitor.start_timemachine_monitoring();
            context.is_timemachine_running = true;
        }

        if matches!(event, Event::TimeMachineBackupFinished) {
            info!("Time Machine backup finished");
            context.is_timemachine_running = false;
            for event in std::mem::take(&mut pending_events) {
                if handle_event(&mut context, event)?.is_break() {
                    break 'outer
                }
            }
            continue;
        }

        if context.is_timemachine_running && event.can_be_delayed() {
            debug!("Time Machine is backing up, delaying event");
            pending_events.insert(event);
        } else if handle_event(&mut context, event)?.is_break() {
            break;
        }
    }

    Ok(())
}

#[derive(PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Event {
    /// Request to reload the configuration
    ReloadConfiguration,
    /// Signal Time machine backup finished
    TimeMachineBackupFinished,
    /// Request to perform the initial scan
    InitialScan,
    /// Request to scan some repositories
    ScanRepositories(BTreeSet<PathBuf>),
    /// Shutdown
    ///
    /// Keep this constant the last one to ensure this event will be the last to
    /// be processed.
    Shutdown,
}

impl Event {
    pub fn can_be_delayed(&self) -> bool {
        match self {
            Event::ReloadConfiguration | Event::TimeMachineBackupFinished | Event::Shutdown => false,
            Event::InitialScan | Event::ScanRepositories(_) => true,
        }
    }
}

struct Monitor {
    control_sender: Sender<MonitorControl>,
    debouncer_control_sender: Sender<DebouncerControl>,
    timemachine_control_sender: Sender<TimeMachineControl>,
    event_receiver_final: Receiver<Event>,
    thread_handles: Vec<JoinHandle<()>>,
    pending_events: BTreeSet<Event>,
}

impl Monitor {
    pub fn new() -> anyhow::Result<Self> {
        let (event_sender_to_debouncer, event_receiver_debouncer) =
            crossbeam_channel::bounded(EVENT_QUEUE_SIZE);
        let (debouncer_thread_handle, debouncer_control_sender, event_receiver_final) =
            monitor_details::spawn_debouncer_thread(event_receiver_debouncer)?;
        let signals_thread_handle = monitor_details::spawn_signals_thread(event_sender_to_debouncer.clone())?;
        let (monitor_thread_handle, monitor_control_sender) =
            monitor_details::spawn_monitor_thread(event_sender_to_debouncer.clone())?;
        let (timemachine_thread_handle, timemachine_control_sender) = 
            monitor_details::spawn_timemachine_thread(event_sender_to_debouncer.clone())?;

        Ok(Self {
            control_sender: monitor_control_sender,
            debouncer_control_sender,
            timemachine_control_sender,
            event_receiver_final,
            thread_handles: vec![
                signals_thread_handle,
                debouncer_thread_handle,
                monitor_thread_handle,
                timemachine_thread_handle,
            ],
            pending_events: BTreeSet::new(),
        })
    }

    pub fn push_event(&mut self, event: Event) {
        self.pending_events.insert(event);
    }

    pub fn get_event(&mut self) -> Option<Event> {
        if let Some(event) = self.pending_events.pop_first() {
            return Some(event)
        }
        
        self.event_receiver_final.recv().ok()
    }

    pub fn set_configuration_file(&mut self, path: impl AsRef<Path>) {
        let _ = self
            .control_sender
            .send(MonitorControl::SetConfigurationFile(
                path.as_ref().to_path_buf(),
            ));
    }

    pub fn set_global_gitignore(&mut self, path: impl AsRef<Path>) {
        let _ = self.control_sender.send(MonitorControl::SetGlobalGitIgnore(
            path.as_ref().to_path_buf(),
        ));
    }

    pub fn set_watched_paths(&mut self, paths: &BTreeSet<PathBuf>) {
        let _ = self
            .control_sender
            .send(MonitorControl::SetWatchedPaths(paths.clone()));
    }

    pub fn set_debounce_duration(&mut self, duration: Duration) {
        let _ = self
            .debouncer_control_sender
            .send(DebouncerControl::SetDebounceDuration(duration));
    }

    pub fn start_timemachine_monitoring(&mut self) {
        let _ = self.timemachine_control_sender.send(TimeMachineControl::ResumeMonitoring);
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        let _ = self.control_sender.send(MonitorControl::Shutdown);
        let _ = self.timemachine_control_sender.send(TimeMachineControl::Shutdown);
        while let Some(handle) = self.thread_handles.pop() {
            handle.join().unwrap();
        }
    }
}

mod monitor_details {
    use std::{
        collections::BTreeSet,
        path::PathBuf,
        thread::JoinHandle,
        time::{Duration, Instant},
    };

    use crossbeam_channel::{Receiver, Sender, select};
    use log::{debug, warn};
    use notify::Watcher;

    use super::EVENT_QUEUE_SIZE;
    use crate::{git, timemachine};

    pub fn spawn_signals_thread(event_sender: Sender<super::Event>) -> anyhow::Result<JoinHandle<()>> {
        let mut signals = signal_hook::iterator::Signals::new([
            signal_hook::consts::SIGTERM,
            signal_hook::consts::SIGINT,
        ])
        .unwrap();
        let thread_handle = std::thread::Builder::new()
            .name("Signals Thread".to_string())
            .spawn(move || {
                debug!("Signals thread starts");
                if signals.into_iter().next().is_some() {
                    let _ = event_sender.send(crate::commands::monitor::Event::Shutdown);
                }
                debug!("Signals thread shutdowns");
            })?;

        Ok(thread_handle)
    }

    pub enum MonitorControl {
        SetWatchedPaths(BTreeSet<PathBuf>),
        SetConfigurationFile(PathBuf),
        SetGlobalGitIgnore(PathBuf),
        Shutdown,
    }

    pub fn spawn_monitor_thread(event_sender: Sender<super::Event>) -> anyhow::Result<(
        JoinHandle<()>,
        Sender<MonitorControl>,
    )> {
        let (control_sender, control_receiver) = crossbeam_channel::bounded(1);
        let (fs_event_sender, fs_event_receiver) = crossbeam_channel::bounded(EVENT_QUEUE_SIZE);
        let watcher_config = notify::Config::default();
        let watcher = notify::RecommendedWatcher::new(fs_event_sender, watcher_config)?;
        let mut watched_paths: BTreeSet<PathBuf> = BTreeSet::new();
        let mut configuration_file_path = None;
        let mut global_gitignore = None;

        let thread_handle = std::thread::Builder::new()
            .name("Monitor Thread".to_string())
            .spawn(move || {
                let mut watcher = watcher;
                debug!("Monitor starts");
                loop {
                    select! {
                        recv(fs_event_receiver) -> event => {
                            if let Ok(Ok(event)) = event
                            {
                                if configuration_file_path.as_ref().is_some_and(|configuration_file_path|{
                                    event.paths.contains(configuration_file_path)
                                })
                                    || global_gitignore
                                        .as_ref()
                                        .is_some_and(|global_gitignore| event.paths.contains(global_gitignore))
                                {
                                    let _ = event_sender.send(crate::commands::monitor::Event::ReloadConfiguration);
                                }

                                if accept_event(&event) {
                                    let repositories = find_repositories(&watched_paths, &event);
                                    let _ = event_sender.send(crate::commands::monitor::Event::ScanRepositories(repositories));
                                }
                            }
                        }
                        recv(control_receiver) -> control => {
                            if let Ok(control) = control {
                                match control {
                                    MonitorControl::SetWatchedPaths(new_paths) => {
                                        let mut paths = watcher.paths_mut();
                                        for path in &watched_paths {
                                            let _ = paths.remove(path);
                                        }
                                        watched_paths.clear();
                                        for path in &new_paths {
                                            if let Ok(()) = paths.add(path, notify::RecursiveMode::Recursive) {
                                                watched_paths.insert(path.clone());
                                            }
                                        }
                                        if let Err(error) = paths.commit() {
                                            warn!("Failed to commit paths to watch: {error}");
                                        }
                                    },
                                    MonitorControl::SetConfigurationFile(path) => {
                                        if let Some(configuration_file_path) = configuration_file_path.take() {
                                            let _ = watcher.unwatch(&configuration_file_path);
                                        }
                                        let _ = watcher.watch(&path, notify::RecursiveMode::NonRecursive);
                                        configuration_file_path = Some(path);
                                    }
                                    MonitorControl::SetGlobalGitIgnore(path) => {
                                        if let Some(global_gitignore) = global_gitignore.take() {
                                            let _ = watcher.unwatch(&global_gitignore);
                                        }
                                        let _ = watcher.watch(&path, notify::RecursiveMode::NonRecursive);
                                        global_gitignore = Some(path);
                                    }
                                    MonitorControl::Shutdown => {
                                        break;
                                    },
                                }
                            }
                        }
                    }
                }
                debug!("Monitor shutdowns");
            })?;

        Ok((thread_handle, control_sender))
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

    pub enum DebouncerControl {
        SetDebounceDuration(Duration),
    }

    pub fn spawn_debouncer_thread(
        input_events: Receiver<super::Event>,
    ) -> anyhow::Result<(
        JoinHandle<()>,
        Sender<DebouncerControl>,
        Receiver<super::Event>,
    )> {
        let (mut output_event_sender, output_event_receiver) =
            crossbeam_channel::bounded(EVENT_QUEUE_SIZE);
        let (debouncer_control_sender, debouncer_control_receiver) = crossbeam_channel::bounded(1);
        let thread_handle = std::thread::Builder::new()
            .name("Debouncer Thread".to_string())
            .spawn(move || {
                fn send_events(events: &mut BTreeSet<super::Event>, sender: &mut Sender<super::Event>) {
                    while let Some(event) = events.pop_first() {
                        let _ = sender.send(event);
                    }
                }

                fn process_control(control: &Result<DebouncerControl, crossbeam_channel::RecvError>, debounce_duration: &mut Duration) {
                    if let Ok(DebouncerControl::SetDebounceDuration(new_debounce_duration)) = control {
                        *debounce_duration = *new_debounce_duration;
                    }
                }

                debug!("Debouncer starts");

                let mut debounce_duration = Duration::from_secs(2);
                let mut debounce_at: Option<Instant> = None;
                let mut events_to_send = BTreeSet::new();

                loop {
                    match debounce_at.and_then(|debounce_at| debounce_at.checked_duration_since(Instant::now())) {
                        Some(timeout) => {
                            select! {
                                recv(input_events) -> event => {
                                    match event {
                                        Ok(super::Event::Shutdown) => {
                                            send_events(&mut events_to_send, &mut output_event_sender);
                                            let _ = output_event_sender.send(super::Event::Shutdown);
                                            break;
                                        }
                                        Ok(event) => {
                                            events_to_send.insert(event);
                                        }
                                        Err(_) => {
                                            send_events(&mut events_to_send, &mut output_event_sender);
                                            break;
                                        }
                                    }
                                }
                                recv(crossbeam_channel::after(timeout)) -> _ => {
                                    debounce_at = None;
                                    send_events(&mut events_to_send, &mut output_event_sender);
                                }
                                recv(debouncer_control_receiver) -> control => {
                                    process_control(&control, &mut debounce_duration);
                                }
                            }
                        }
                        None => {
                            select! {
                                recv(input_events) -> event => {
                                    match event {
                                        Ok(super::Event::Shutdown) => {
                                            send_events(&mut events_to_send, &mut output_event_sender);
                                            let _ = output_event_sender.send(super::Event::Shutdown);
                                            break;
                                        }
                                        Ok(event) => {
                                            if debounce_at.is_none() {
                                                debounce_at = Some(Instant::now() + debounce_duration);
                                                events_to_send.insert(event);
                                            }
                                        }
                                        Err(_) => {
                                            send_events(&mut events_to_send, &mut output_event_sender);
                                            break;
                                        },
                                    }
                                }
                                recv(debouncer_control_receiver) -> control => {
                                    process_control(&control, &mut debounce_duration);
                                }
                            }
                        }
                    }
                }

                debug!("Debouncer shutdowns");
            })?;

        Ok((
            thread_handle,
            debouncer_control_sender,
            output_event_receiver,
        ))
    }

    pub enum TimeMachineControl {
        ResumeMonitoring,
        Shutdown,
    }

    pub fn spawn_timemachine_thread(event_sender: Sender<super::Event>) -> anyhow::Result<(JoinHandle<()>, Sender<TimeMachineControl>)> {
        let (control_sender, control_receiver) = crossbeam_channel::bounded(EVENT_QUEUE_SIZE);
        let thread_handle = std::thread::Builder::new().name("Time Machine Monitoring Thread".to_string()).spawn(move|| {
            debug!("Time Machine monitoring thread started");
            'outer: for control in &control_receiver {
                match control {
                    TimeMachineControl::ResumeMonitoring => {
                        const TIMEOUT: Duration = Duration::from_secs(1);
                        debug!("Start monitoring tmutil status");
                        while timemachine::is_time_machine_running() {
                            if let Ok(TimeMachineControl::Shutdown) = control_receiver.recv_timeout(TIMEOUT) {
                                break 'outer;
                            }
                        }
                        debug!("Stop monitoring tmutil status");
                        let _ = event_sender.send(super::Event::TimeMachineBackupFinished);
                    },
                    TimeMachineControl::Shutdown => {
                        break;
                    },
                }
            }
            debug!("Time Machine monitoring thread stopped");
        })?;

        Ok((thread_handle, control_sender))
    }

    #[cfg(test)]
    mod tests {
        use rstest::rstest;
        use std::time::Duration;

        use crate::commands::monitor::{Event, monitor_details::DebouncerControl};

        #[rstest]
        #[case(notify::Event::default().set_kind(notify::EventKind::Create(notify::event::CreateKind::File)), true)]
        #[case(notify::Event::default().set_kind(notify::EventKind::Remove(notify::event::RemoveKind::File)), true)]
        #[case(notify::Event::default().set_kind(notify::EventKind::Modify(notify::event::ModifyKind::Data(notify::event::DataChange::Content))).add_path(".gitignore".into()), true)]
        #[case(notify::Event::default().set_kind(notify::EventKind::Modify(notify::event::ModifyKind::Data(notify::event::DataChange::Content))).add_path("yop".into()), false)]
        #[case(notify::Event::default().set_kind(notify::EventKind::Modify(notify::event::ModifyKind::Name(notify::event::RenameMode::From))), true)]
        #[case(notify::Event::default().set_kind(notify::EventKind::Access(notify::event::AccessKind::Read)), false)]
        #[case(notify::Event::default().set_kind(notify::EventKind::Other), false)]
        fn test_accept_event(#[case] event: notify::Event, #[case] accepted: bool) {
            let result = super::accept_event(&event);

            assert_eq!(accepted, result);
        }

        #[test]
        fn test_spawn_debouncer_thread_shutdown() {
            let (input_sender, input_receiver) = crossbeam_channel::bounded(4);
            let (thread_handle, _control_sender, output_receiver) =
                super::spawn_debouncer_thread(input_receiver).unwrap();

            input_sender.send(Event::Shutdown).unwrap();
            let output_event = output_receiver.recv().unwrap();
            assert_eq!(Event::Shutdown, output_event);
            thread_handle.join().unwrap();
        }

        #[test]
        fn test_spawn_debouncer_thread_input_dropped() {
            let (input_sender, input_receiver) = crossbeam_channel::bounded(4);
            let (thread_handle, _control_sender, _output_receiver) =
                super::spawn_debouncer_thread(input_receiver).unwrap();
            drop(input_sender);
            thread_handle.join().unwrap();
        }

        #[test]
        fn test_spawn_debouncer_thread_input_dropped_during_debounce() {
            let (input_sender, input_receiver) = crossbeam_channel::bounded(4);
            let (thread_handle, control_sender, _output_receiver) =
                super::spawn_debouncer_thread(input_receiver).unwrap();
            control_sender
                .send(DebouncerControl::SetDebounceDuration(Duration::from_secs(
                    2,
                )))
                .unwrap();
            input_sender.send(Event::ReloadConfiguration).unwrap();
            drop(input_sender);
            thread_handle.join().unwrap();
        }

        #[test]
        fn test_spawn_debouncer_thread_control_during_debounce() {
            let (input_sender, input_receiver) = crossbeam_channel::bounded(4);
            let (thread_handle, control_sender, _output_receiver) =
                super::spawn_debouncer_thread(input_receiver).unwrap();
            control_sender
                .send(DebouncerControl::SetDebounceDuration(Duration::from_secs(
                    2,
                )))
                .unwrap();
            input_sender.send(Event::ReloadConfiguration).unwrap();
            control_sender
                .send(DebouncerControl::SetDebounceDuration(Duration::from_secs(
                    2,
                )))
                .unwrap();
            input_sender.send(Event::Shutdown).unwrap();
            thread_handle.join().unwrap();
        }

        #[test]
        fn test_spawn_debouncer_thread_reload_event_is_debounced() {
            let (input_sender, input_receiver) = crossbeam_channel::bounded(4);
            let (thread_handle, control_sender, output_receiver) =
                super::spawn_debouncer_thread(input_receiver).unwrap();
            let debounce_duration = Duration::from_millis(250);
            control_sender
                .send(DebouncerControl::SetDebounceDuration(debounce_duration))
                .unwrap();
            input_sender.send(Event::ReloadConfiguration).unwrap();
            input_sender.send(Event::ReloadConfiguration).unwrap();
            // Sleep enough to ensure the debouncer releases events
            std::thread::sleep(debounce_duration);
            let output_event = output_receiver.recv().unwrap();
            assert_eq!(Event::ReloadConfiguration, output_event);
            assert!(output_receiver.recv_timeout(debounce_duration).is_err());
            input_sender.send(Event::Shutdown).unwrap();
            thread_handle.join().unwrap();
        }

        #[test]
        fn test_spawn_debouncer_thread_reload_event_is_debounced_early_shutdown() {
            let (input_sender, input_receiver) = crossbeam_channel::bounded(4);
            let (thread_handle, control_sender, output_receiver) =
                super::spawn_debouncer_thread(input_receiver).unwrap();
            let debounce_duration = Duration::from_millis(250);
            control_sender
                .send(DebouncerControl::SetDebounceDuration(debounce_duration))
                .unwrap();
            input_sender.send(Event::ReloadConfiguration).unwrap();
            input_sender.send(Event::ReloadConfiguration).unwrap();
            input_sender.send(Event::Shutdown).unwrap();
            let reload_event = output_receiver.recv().unwrap();
            let shutdown_event = output_receiver.recv().unwrap();
            thread_handle.join().unwrap();
            assert_eq!(Event::ReloadConfiguration, reload_event);
            assert_eq!(Event::Shutdown, shutdown_event);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serial_test::serial;
    use temp_dir_builder::TempDirectoryBuilder;

    use crate::{cache::Cache, json::save_json_file};

    /// Test the behavior in case of a missing configuration file.
    /// It should not return an error, it should create the default configuration file.
    #[test]
    #[serial]
    fn test_config_file_does_not_exist() {
        let temp_dir = TempDirectoryBuilder::default().build().unwrap();
        let config_file_path = temp_dir.path().join("non_existent_file.config");
        let thread_handle = std::thread::spawn(move || {
            let mut cache = Cache::open_in_memory().unwrap();
            super::execute(&config_file_path, None, &mut cache, true, false).unwrap();
        });
        // Ensure the signals handlers are setup
        std::thread::sleep(Duration::from_secs(5));
        crate::commands::tests::send_sigint();
        thread_handle.join().unwrap();
    }

    #[test]
    #[serial]
    fn test_initial_scan() {
        let temp_dir = TempDirectoryBuilder::default()
            .add_directory("folder/repository")
            .add_text_file("folder/repository/.gitignore", "a\nb\nc")
            .add_empty_file("folder/repository/a")
            .add_empty_file("folder/repository/b")
            .add_empty_file("folder/repository/c")
            .build()
            .unwrap();
        let folder_path = temp_dir.path().join("folder");
        let repository_path = folder_path.join("repository");
        let config_file_path = folder_path.join("config.json");
        let config = crate::commands::tests::create_config(&folder_path);
        save_json_file(&config_file_path, &config).unwrap();
        crate::commands::tests::init_git_repository(&repository_path);
        let thread_handle = std::thread::spawn(move||{
            let mut cache = Cache::open_in_memory().unwrap();

            super::execute(&config_file_path, None, &mut cache, false, true).unwrap();

            cache
        });
        // Need to wait to ensure the internal Monitor is created by super::execute() to ensure
        // the signal will be handled.
        std::thread::sleep(Duration::from_secs(5));
        crate::commands::tests::send_sigint();
        let cache = thread_handle.join().unwrap();
        let paths = cache.paths();
        assert_eq!(paths.len(), 3);
    }
}
