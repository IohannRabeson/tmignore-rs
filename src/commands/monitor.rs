use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    thread::JoinHandle,
    time::Duration,
};

use crossbeam_channel::{Receiver, Sender};
use log::{debug, warn};

use crate::{
    cache::Cache,
    commands::{
        TimeMachine,
        monitor::monitor_details::{DebouncerControl, MonitorControl},
    },
    config::Config,
};

const EVENT_QUEUE_SIZE: usize = 128;

pub fn execute(
    config_file_path: impl AsRef<Path>,
    global_gitignore_path: Option<PathBuf>,
    cache: &mut Cache,
    dry_run: bool,
    details: bool,
) -> Result<(), anyhow::Error> {
    let config_file_path = config_file_path.as_ref().canonicalize()?.clone();
    let mut config = Config::load_or_create_file(&config_file_path)?;

    super::run::execute(&config, cache, dry_run, details)?;

    let mut whitelist = super::create_whitelist(&config.whitelist_patterns)?;
    let mut monitor = Monitor::new(&config_file_path, global_gitignore_path)?;

    monitor.set_watched_paths(&config.search_directories);
    monitor.set_debounce_duration(config.debounce_duration);

    while let Some(event) = monitor.get_event() {
        match event {
            Event::ReloadConfiguration => match config.reload_file(&config_file_path) {
                Ok(()) => {
                    whitelist = super::create_whitelist(&config.whitelist_patterns)?;
                    monitor.set_watched_paths(&config.search_directories);
                    monitor.set_debounce_duration(config.debounce_duration);
                    debug!("Configuration reloaded");
                    super::run::execute(&config, cache, dry_run, details)?;
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
                    debug!("Scanning repository '{}'", repository_to_scan.display());
                    let mut exclusions = BTreeSet::new();
                    super::find_paths_to_exclude_from_backup(
                        repository_to_scan,
                        &whitelist,
                        &mut exclusions,
                    )?;
                    let diff = cache.find_diff_in_directory(&exclusions, repository_to_scan);
                    let paths_failed_to_add =
                        super::apply_diff_and_print::<TimeMachine>(&diff, dry_run, details);

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

    Ok(())
}

#[derive(PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Event {
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

struct Monitor {
    control_sender: Sender<MonitorControl>,
    debouncer_control_sender: Sender<DebouncerControl>,
    event_receiver_final: Receiver<Event>,
    thread_handles: Vec<JoinHandle<()>>,
}

impl Monitor {
    pub fn new(
        configuration_file_path: &Path,
        global_gitignore: Option<PathBuf>,
    ) -> anyhow::Result<Self> {
        let (event_sender_to_debouncer, event_receiver_debouncer) =
            crossbeam_channel::bounded(EVENT_QUEUE_SIZE);
        let (debouncer_thread_handle, debouncer_control_sender, event_receiver_final) =
            monitor_details::spawn_debouncer_thread(event_receiver_debouncer)?;
        let (signals_thread_handle, signals_receiver) = monitor_details::spawn_signals_thread()?;
        let (monitor_thread_handle, monitor_control_sender, event_receiver) =
            monitor_details::spawn_monitor_thread(configuration_file_path, global_gitignore)?;
        let dispatcher_thread_handle = monitor_details::spawn_dispatcher_thread(signals_receiver, event_receiver, event_sender_to_debouncer)?;

        Ok(Self {
            control_sender: monitor_control_sender,
            debouncer_control_sender,
            event_receiver_final,
            thread_handles: vec![signals_thread_handle, dispatcher_thread_handle, debouncer_thread_handle, monitor_thread_handle],
        })
    }

    pub fn get_event(&mut self) -> Option<Event> {
        self.event_receiver_final.recv().ok()
    }

    pub fn set_watched_paths(&mut self, paths: &BTreeSet<PathBuf>) {
        let _ = self
            .control_sender
            .send(MonitorControl::SetPaths(paths.clone()));
    }

    pub fn set_debounce_duration(&mut self, duration: Duration) {
        let _ = self
            .debouncer_control_sender
            .send(DebouncerControl::SetDebounceDuration(duration));
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        let _ = self.control_sender.send(MonitorControl::Shutdown);

        while let Some(handle) = self.thread_handles.pop() {
            handle.join().unwrap();
        }
    }
}

mod monitor_details {
    use std::{
        collections::BTreeSet,
        path::{Path, PathBuf},
        thread::JoinHandle,
        time::{Duration, Instant},
    };

    use crossbeam_channel::{Receiver, Sender, select};
    use log::{debug, warn};
    use notify::Watcher;

    use crate::git;
    use super::EVENT_QUEUE_SIZE;

    pub fn spawn_signals_thread() -> anyhow::Result<(JoinHandle<()>, Receiver<()>)> {
        let mut signals = signal_hook::iterator::Signals::new([
            signal_hook::consts::SIGTERM,
            signal_hook::consts::SIGINT,
        ])
        .unwrap();
        let (signal_sender, signal_receiver) = crossbeam_channel::bounded(1);
        let thread_handle = std::thread::Builder::new()
            .name("Signals Thread".to_string())
            .spawn(move || {
                debug!("Signals thread starts");
                if signals.into_iter().next().is_some() {
                    let _ = signal_sender.send(());
                }
                debug!("Signals thread shutdowns");
            })?;

        Ok((thread_handle, signal_receiver))
    }

    pub enum MonitorControl {
        SetPaths(BTreeSet<PathBuf>),
        Shutdown,
    }

    pub fn spawn_monitor_thread(
        configuration_file_path: &Path,
        global_gitignore: Option<PathBuf>,
    ) -> anyhow::Result<(
        JoinHandle<()>,
        Sender<MonitorControl>,
        Receiver<super::Event>,
    )> {
        let configuration_file_path = configuration_file_path.canonicalize()?;
        let (event_sender, event_receiver) = crossbeam_channel::bounded(EVENT_QUEUE_SIZE);
        let (control_sender, control_receiver) = crossbeam_channel::bounded(1);
        let (fs_event_sender, fs_event_receiver) = crossbeam_channel::bounded(EVENT_QUEUE_SIZE);
        let watcher_config = notify::Config::default();
        let mut watcher = notify::RecommendedWatcher::new(fs_event_sender, watcher_config)?;
        let mut watched_paths: BTreeSet<PathBuf> = BTreeSet::new();

        watcher.watch(
            &configuration_file_path,
            notify::RecursiveMode::NonRecursive,
        )?;
        if let Some(path) = global_gitignore.as_ref() {
            watcher.watch(path, notify::RecursiveMode::NonRecursive)?;
        }

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
                                if event.paths.contains(&configuration_file_path)
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
                                    MonitorControl::SetPaths(new_paths) => {
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

        Ok((thread_handle, control_sender, event_receiver))
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

    pub fn spawn_dispatcher_thread(signals_receiver: Receiver<()>, event_receiver: Receiver<super::Event>, event_sender_to_debouncer: Sender<super::Event>) -> anyhow::Result<JoinHandle<()>> {
        let thread_handle = std::thread::Builder::new()
            .name("Dispatcher Thread".to_string())
            .spawn(move || {
                debug!("Dispatcher starts");
                loop {
                    select! {
                        recv(signals_receiver) -> event => {
                            if let Ok(()) = event {
                                let _ = event_sender_to_debouncer.send(super::Event::Shutdown);
                                break;
                            }
                        }
                        recv(event_receiver) -> event => {
                            if let Ok(event) = event {
                                let _ = event_sender_to_debouncer.send(event);
                            }
                        }
                    }
                }
                debug!("Dispatcher shutdowns");
            })?;

        Ok(thread_handle)
    }
    #[cfg(test)]
    mod tests {
        use rstest::rstest;

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
    }
}
