use std::{
    collections::{BTreeMap, BTreeSet},
    ops::ControlFlow,
    path::{Path, PathBuf},
    thread::JoinHandle,
    time::Duration,
};

use anyhow::Context;
use crossbeam_channel::{Receiver, Sender};
use log::{debug, error, info, warn};
use regex::RegexSet;

use crate::{
    cache::Cache,
    commands::{
        TimeMachine,
        monitor::monitor_details::{DebouncerControl, MonitorControl, TimeMachineControl},
    },
    config::Config,
    diff::Diff,
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
                context
                    .monitor
                    .set_watched_paths(&context.config.search_directories);
                context
                    .monitor
                    .set_debounce_duration(context.config.debounce_duration);
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
            super::run::execute(
                context.config,
                context.cache,
                context.dry_run,
                context.details,
            )?;
        }
        Event::ScanPaths(dirty_paths) => {
            let repositories_to_scan = find_repositories_to_scan(
                &dirty_paths,
                &context.config.search_directories,
                context.cache,
            )?;
            for repository_to_scan in &repositories_to_scan {
                debug!("Scanning repository '{}'", repository_to_scan.display());
                let mut exclusions = Vec::new();
                super::find_paths_to_exclude_from_backup(
                    repository_to_scan,
                    context.whitelist,
                    &mut exclusions,
                )?;
                exclusions.sort_unstable();
                exclusions.dedup();

                let mut cached_paths = context.cache.paths_created_by(repository_to_scan)?;
                cached_paths.sort_unstable();

                let diff = Diff::from_sorted(&exclusions, &cached_paths);

                if diff.added.is_empty() && diff.removed.is_empty() {
                    debug!(
                        "No changes in repository '{}'",
                        repository_to_scan.display()
                    );
                    continue;
                }

                let paths_failed_to_add = super::apply_diff_and_print::<TimeMachine>(
                    &diff,
                    context.dry_run,
                    context.details,
                );

                if !context.dry_run {
                    if !diff.removed.is_empty() {
                        context
                            .cache
                            .remove_paths(diff.removed.iter(), repository_to_scan)?;
                    }
                    let paths_to_add = diff
                        .added
                        .iter()
                        .filter(|path| !paths_failed_to_add.contains(*path))
                        .map(|path| {
                            crate::diff::Exclusion::new(path.clone(), repository_to_scan.clone())
                        });
                    context.cache.add_paths(paths_to_add)?;
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

/// Search the repositories related to some paths.
/// The repositories listed are in one of the search directories.
///
/// A path is skipped only when both of these hold, because each one alone leaves a way for the
/// scan of the repository to report something new:
/// - a cached exclusion already covers one of its ancestors: otherwise the path is a new entry of
///   `git ls-files`, so a new exclusion to add. A path that was deleted is covered by this too:
///   an exclusion to remove is never covered by another one, because `git ls-files --directory`
///   collapses, so the cache never holds both a directory and something under it;
/// - it is ignored: a path that is not ignored stops `git ls-files --directory` from collapsing
///   its directory, so the exclusion of that directory must be removed.
///
/// The repository is scanned as soon as one of its paths is not skipped.
fn find_repositories_to_scan(
    paths: &BTreeSet<PathBuf>,
    search_directories: &BTreeSet<PathBuf>,
    cache: &Cache,
) -> Result<BTreeSet<PathBuf>, anyhow::Error> {
    let mut paths_by_repository: BTreeMap<PathBuf, Vec<&Path>> = BTreeMap::new();

    for path in paths {
        if let Some(repository_path) = crate::git::find_parent_repository(path)
            && search_directories
                .iter()
                .any(|search_directory| repository_path.starts_with(search_directory))
        {
            paths_by_repository
                .entry(repository_path)
                .or_default()
                .push(path.as_path());
        }
    }

    let mut repositories = BTreeSet::new();

    for (repository_path, repository_paths) in paths_by_repository {
        let mut scan = false;

        for path in &repository_paths {
            if !cache.contains_ancestor_of(path)? {
                scan = true;
                break;
            }
        }

        // Checking whether the paths are ignored costs a git process, so it runs once for the
        // whole batch and only when the cheap checks did not already settle the repository.
        if scan || crate::git::contains_not_ignored_path(&repository_path, &repository_paths) {
            repositories.insert(repository_path);
        }
    }

    Ok(repositories)
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
    let mut pending_scan_paths = BTreeSet::new();

    'outer: while let Some(event) = context.monitor.get_event() {
        if !context.is_timemachine_running
            && event.can_be_delayed()
            && is_time_machine_running_logged()
        {
            info!("Time Machine backup started");
            context.monitor.start_timemachine_monitoring();
            context.is_timemachine_running = true;
        }

        if matches!(event, Event::TimeMachineBackupFinished) {
            info!("Time Machine backup finished");
            context.is_timemachine_running = false;
            if !pending_scan_paths.is_empty() {
                pending_events.insert(Event::ScanPaths(std::mem::take(&mut pending_scan_paths)));
            }
            for event in std::mem::take(&mut pending_events) {
                if handle_event(&mut context, event)?.is_break() {
                    break 'outer;
                }
            }
            continue;
        }

        if context.is_timemachine_running && event.can_be_delayed() {
            debug!("Time Machine is backing up, delaying event");
            match event {
                Event::ScanPaths(paths) => {
                    pending_scan_paths.extend(paths);
                }
                event => {
                    pending_events.insert(event);
                }
            }
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
    /// Request to scan the repositories containing some paths
    ScanPaths(BTreeSet<PathBuf>),
    /// Shutdown
    ///
    /// Keep this constant the last one to ensure this event will be the last to
    /// be processed.
    Shutdown,
}

impl Event {
    pub fn can_be_delayed(&self) -> bool {
        match self {
            Event::ReloadConfiguration | Event::TimeMachineBackupFinished | Event::Shutdown => {
                false
            }
            Event::InitialScan | Event::ScanPaths(_) => true,
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
        let signals_thread_handle =
            monitor_details::spawn_signals_thread(event_sender_to_debouncer.clone())?;
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
            return Some(event);
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
        let (registered_sender, registered_receiver) = crossbeam_channel::bounded(1);
        let _ = self.control_sender.send(MonitorControl::SetWatchedPaths(
            paths.clone(),
            registered_sender,
        ));
        let _ = registered_receiver.recv();
    }

    pub fn set_debounce_duration(&mut self, duration: Duration) {
        let _ = self
            .debouncer_control_sender
            .send(DebouncerControl::SetDebounceDuration(duration));
    }

    pub fn start_timemachine_monitoring(&mut self) {
        let _ = self
            .timemachine_control_sender
            .send(TimeMachineControl::ResumeMonitoring);
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        let _ = self.control_sender.send(MonitorControl::Shutdown);
        let _ = self
            .timemachine_control_sender
            .send(TimeMachineControl::Shutdown);
        while let Some(handle) = self.thread_handles.pop() {
            if let Err(error) = super::join_thread(handle) {
                error!("Failed to join thread: {error}");
            }
        }
    }
}

fn is_time_machine_running_logged() -> bool {
    log_status(crate::timemachine::is_time_machine_running())
}

fn log_status(status: anyhow::Result<bool>) -> bool {
    match status {
        Ok(running) => running,
        Err(error) => {
            warn!("Failed to query Time Machine status: {error}");
            false
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

    use anyhow::Context;
    use crossbeam_channel::{Receiver, Sender, select};
    use log::debug;
    use notify::Watcher;

    use super::EVENT_QUEUE_SIZE;

    pub fn spawn_signals_thread(
        event_sender: Sender<super::Event>,
    ) -> anyhow::Result<JoinHandle<()>> {
        let mut signals = signal_hook::iterator::Signals::new([
            signal_hook::consts::SIGTERM,
            signal_hook::consts::SIGINT,
        ])
        .context("Failed to setup signals hooks")?;

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
        SetWatchedPaths(BTreeSet<PathBuf>, Sender<()>),
        SetConfigurationFile(PathBuf),
        SetGlobalGitIgnore(PathBuf),
        Shutdown,
    }

    pub fn spawn_monitor_thread(
        event_sender: Sender<super::Event>,
    ) -> anyhow::Result<(JoinHandle<()>, Sender<MonitorControl>)> {
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
                                    let _ = event_sender.send(crate::commands::monitor::Event::ScanPaths(event.paths.into_iter().collect()));
                                }
                            }
                        }
                        recv(control_receiver) -> control => {
                            if let Ok(control) = control {
                                match control {
                                    MonitorControl::SetWatchedPaths(new_paths, registered) => {
                                        for path in &watched_paths {
                                            let _ = watcher.unwatch(path);
                                        }
                                        watched_paths.clear();
                                        for path in new_paths {
                                            if let Ok(()) = watcher.watch(&path, notify::RecursiveMode::Recursive) {
                                                watched_paths.insert(path);
                                            }
                                        }
                                        let _ = registered.send(());
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
                fn send_events(events: &mut BTreeSet<super::Event>, paths_to_scan: &mut BTreeSet<PathBuf>, sender: &mut Sender<super::Event>) {
                    if !paths_to_scan.is_empty() {
                        events.insert(super::Event::ScanPaths(std::mem::take(paths_to_scan)));
                    }
                    while let Some(event) = events.pop_first() {
                        let _ = sender.send(event);
                    }
                }

                fn collect_event(event: super::Event, events: &mut BTreeSet<super::Event>, paths_to_scan: &mut BTreeSet<PathBuf>) {
                    match event {
                        super::Event::ScanPaths(paths) => {
                            paths_to_scan.extend(paths);
                        }
                        event => {
                            events.insert(event);
                        }
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
                let mut paths_to_scan = BTreeSet::new();

                loop {
                    if let Some(timeout) = debounce_at.and_then(|debounce_at| debounce_at.checked_duration_since(Instant::now())) {
                        select! {
                            recv(input_events) -> event => {
                                match event {
                                    Ok(super::Event::Shutdown) => {
                                        send_events(&mut events_to_send, &mut paths_to_scan, &mut output_event_sender);
                                        let _ = output_event_sender.send(super::Event::Shutdown);
                                        break;
                                    }
                                    Ok(event) => {
                                        collect_event(event, &mut events_to_send, &mut paths_to_scan);
                                    }
                                    Err(_) => {
                                        send_events(&mut events_to_send, &mut paths_to_scan, &mut output_event_sender);
                                        break;
                                    }
                                }
                            }
                            recv(crossbeam_channel::after(timeout)) -> _ => {
                                debounce_at = None;
                                send_events(&mut events_to_send, &mut paths_to_scan, &mut output_event_sender);
                            }
                            recv(debouncer_control_receiver) -> control => {
                                process_control(&control, &mut debounce_duration);
                            }
                        }
                    } else {
                        if debounce_at.is_some() {
                            debounce_at = None;
                            send_events(&mut events_to_send, &mut paths_to_scan, &mut output_event_sender);
                        }
                        select! {
                            recv(input_events) -> event => {
                                match event {
                                    Ok(super::Event::Shutdown) => {
                                        send_events(&mut events_to_send, &mut paths_to_scan, &mut output_event_sender);
                                        let _ = output_event_sender.send(super::Event::Shutdown);
                                        break;
                                    }
                                    Ok(event) => {
                                        // If debounce_duration is too big, it will debounce immediatly.
                                        // This should never happens in practise because we check this value is not too big when validating the config.
                                        debounce_at = Some(Instant::now().checked_add(debounce_duration).unwrap_or(Instant::now()));
                                        collect_event(event, &mut events_to_send, &mut paths_to_scan);
                                    }
                                    Err(_) => {
                                        send_events(&mut events_to_send, &mut paths_to_scan, &mut output_event_sender);
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

    pub fn spawn_timemachine_thread(
        event_sender: Sender<super::Event>,
    ) -> anyhow::Result<(JoinHandle<()>, Sender<TimeMachineControl>)> {
        let (control_sender, control_receiver) = crossbeam_channel::bounded(EVENT_QUEUE_SIZE);
        let thread_handle = std::thread::Builder::new()
            .name("Time Machine Monitoring Thread".to_string())
            .spawn(move || {
                debug!("Time Machine monitoring thread started");
                'outer: for control in &control_receiver {
                    match control {
                        TimeMachineControl::ResumeMonitoring => {
                            const TIMEOUT: Duration = Duration::from_secs(1);
                            debug!("Start monitoring tmutil status");
                            while super::is_time_machine_running_logged() {
                                if let Ok(TimeMachineControl::Shutdown) =
                                    control_receiver.recv_timeout(TIMEOUT)
                                {
                                    break 'outer;
                                }
                            }
                            debug!("Stop monitoring tmutil status");
                            let _ = event_sender.send(super::Event::TimeMachineBackupFinished);
                        }
                        TimeMachineControl::Shutdown => {
                            break;
                        }
                    }
                }
                debug!("Time Machine monitoring thread stopped");
            })?;

        Ok((thread_handle, control_sender))
    }

    #[cfg(test)]
    mod tests {
        use rstest::rstest;
        use std::{collections::BTreeSet, path::PathBuf, time::Duration};

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
        fn test_spawn_debouncer_thread_scan_paths_are_merged() {
            let (input_sender, input_receiver) = crossbeam_channel::bounded(4);
            let (thread_handle, control_sender, output_receiver) =
                super::spawn_debouncer_thread(input_receiver).unwrap();
            let debounce_duration = Duration::from_millis(250);
            control_sender
                .send(DebouncerControl::SetDebounceDuration(debounce_duration))
                .unwrap();
            input_sender
                .send(Event::ScanPaths(BTreeSet::from([PathBuf::from("/a")])))
                .unwrap();
            input_sender
                .send(Event::ScanPaths(BTreeSet::from([PathBuf::from("/b")])))
                .unwrap();
            std::thread::sleep(debounce_duration);
            let output_event = output_receiver.recv().unwrap();
            assert_eq!(
                Event::ScanPaths(BTreeSet::from([PathBuf::from("/a"), PathBuf::from("/b")])),
                output_event
            );
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
    use std::{
        collections::BTreeSet,
        path::{Path, PathBuf},
        time::Duration,
    };

    use rstest::rstest;
    use serial_test::serial;
    use temp_dir_builder::TempDirectoryBuilder;

    use crate::{cache::Cache, commands::tests::run_git, config::Config, json::save_json_file};

    fn rescan(
        cache: &mut Cache,
        mut config: Config,
        temp_dir_path: &Path,
        changed_paths: BTreeSet<PathBuf>,
    ) -> BTreeSet<PathBuf> {
        let mut whitelist = super::super::create_whitelist(&config.whitelist_patterns).unwrap();
        let mut monitor = super::Monitor::new().unwrap();
        let config_file_path = temp_dir_path.join("config.json");
        let mut context = super::HandleEventContext {
            config: &mut config,
            config_file_path: &config_file_path,
            whitelist: &mut whitelist,
            monitor: &mut monitor,
            cache,
            dry_run: false,
            details: false,
            is_timemachine_running: false,
        };

        let _ = super::handle_event(&mut context, super::Event::ScanPaths(changed_paths)).unwrap();

        let cached_paths = cache.paths().unwrap().into_iter().collect();

        crate::commands::tests::send_sigint();
        drop(monitor);

        cached_paths
    }

    #[test]
    #[serial]
    fn test_set_watched_paths_registers_the_watch_before_returning() {
        let temp_dir = TempDirectoryBuilder::default().build().unwrap();
        let temp_dir_path = temp_dir.path().canonicalize().unwrap();
        let mut monitor = super::Monitor::new().unwrap();

        monitor.set_debounce_duration(Duration::from_millis(100));
        monitor.set_watched_paths(&BTreeSet::from([temp_dir_path.clone()]));

        let created_path = temp_dir_path.join("created");
        std::fs::write(&created_path, "").unwrap();

        // Read the channel directly instead of calling get_event because it blocks without a timeout, so
        // a regression would hang the suite instead of failing it.
        let event = monitor
            .event_receiver_final
            .recv_timeout(Duration::from_secs(10));

        crate::commands::tests::send_sigint();
        drop(monitor);

        match event {
            Ok(super::Event::ScanPaths(paths)) => assert!(
                paths.contains(&created_path),
                "the watcher reported an event, but not for the created path: {paths:?}"
            ),
            other => panic!(
                "a path created right after set_watched_paths returned was not reported, so the \
                 watch was not registered yet when it returned: {other:?}"
            ),
        }
    }

    fn test_iterations(default: usize) -> usize {
        match std::env::var("TMIGNORE_RS_TEST_ITERATIONS") {
            Ok(value) => value.parse().unwrap_or_else(|error| {
                panic!("TMIGNORE_RS_TEST_ITERATIONS='{value}' is not a valid usize: {error}")
            }),
            Err(_) => default,
        }
    }

    fn commit_all(repository_path: &Path, message: &str) {
        run_git(&["-C", repository_path.to_str().unwrap(), "add", "-A"]);
        run_git(&[
            "-C",
            repository_path.to_str().unwrap(),
            "-c",
            "user.email=a@b.c",
            "-c",
            "user.name=a",
            "commit",
            "-q",
            "-m",
            message,
        ]);
    }

    #[test]
    #[serial]
    fn test_rescan_main_repository_does_not_remove_submodule_exclusions() {
        let temp_dir = TempDirectoryBuilder::default().build().unwrap();
        let temp_dir_path = temp_dir.path().canonicalize().unwrap();
        let sub_source_path = temp_dir_path.join("sub_source");
        let main_path = temp_dir_path.join("main");

        crate::commands::tests::init_git_repository(&sub_source_path);
        std::fs::write(sub_source_path.join(".gitignore"), "ignored_in_sub\n").unwrap();
        commit_all(&sub_source_path, "init submodule source");

        crate::commands::tests::init_git_repository(&main_path);
        std::fs::write(main_path.join(".gitignore"), "ignored_in_main\n").unwrap();
        commit_all(&main_path, "init main repository");
        run_git(&[
            "-c",
            "protocol.file.allow=always",
            "-C",
            main_path.to_str().unwrap(),
            "submodule",
            "add",
            "-q",
            sub_source_path.to_str().unwrap(),
            "submodule",
        ]);
        commit_all(&main_path, "add submodule");

        std::fs::write(main_path.join("submodule").join("ignored_in_sub"), "").unwrap();
        std::fs::write(main_path.join("ignored_in_main"), "").unwrap();

        let mut cache = Cache::open_in_memory().unwrap();
        let config = crate::commands::tests::create_config(&main_path);

        super::super::run::execute(&config, &mut cache, false, false).unwrap();

        let submodule_ignored_path = main_path.join("submodule").join("ignored_in_sub");
        let main_ignored_path = main_path.join("ignored_in_main");

        let cached_paths: BTreeSet<_> = cache.paths().unwrap().into_iter().collect();
        assert!(
            cached_paths.contains(&submodule_ignored_path),
            "the initial scan should have excluded the submodule's ignored file"
        );
        assert!(cached_paths.contains(&main_ignored_path));

        std::fs::write(main_path.join(".gitignore"), "ignored_in_main\n\n").unwrap();

        let cached_paths = rescan(
            &mut cache,
            config,
            &temp_dir_path,
            BTreeSet::from([main_path.join(".gitignore")]),
        );

        assert!(
            cached_paths.contains(&submodule_ignored_path),
            "rescanning the main repository must not remove the submodule's exclusions"
        );
    }

    #[test]
    #[serial]
    fn test_rescan_main_repository_does_not_remove_nested_worktree_exclusions() {
        let temp_dir = TempDirectoryBuilder::default().build().unwrap();
        let temp_dir_path = temp_dir.path().canonicalize().unwrap();
        let main_path = temp_dir_path.join("main");

        crate::commands::tests::init_git_repository(&main_path);
        std::fs::write(main_path.join(".gitignore"), "ignored_in_main\n").unwrap();
        commit_all(&main_path, "init main repository");
        run_git(&["-C", main_path.to_str().unwrap(), "branch", "feature"]);
        run_git(&[
            "-C",
            main_path.to_str().unwrap(),
            "worktree",
            "add",
            "-q",
            "nested_worktree",
            "feature",
        ]);

        let worktree_path = main_path.join("nested_worktree");
        std::fs::write(worktree_path.join(".gitignore"), "ignored_in_worktree\n").unwrap();
        commit_all(&worktree_path, "add gitignore in worktree");

        std::fs::write(worktree_path.join("ignored_in_worktree"), "").unwrap();
        std::fs::write(main_path.join("ignored_in_main"), "").unwrap();

        let mut cache = Cache::open_in_memory().unwrap();
        let config = crate::commands::tests::create_config(&main_path);

        super::super::run::execute(&config, &mut cache, false, false).unwrap();

        let worktree_ignored_path = worktree_path.join("ignored_in_worktree");
        let main_ignored_path = main_path.join("ignored_in_main");

        let cached_paths: BTreeSet<_> = cache.paths().unwrap().into_iter().collect();
        assert!(
            cached_paths.contains(&worktree_ignored_path),
            "the initial scan should have excluded the worktree's ignored file"
        );
        assert!(cached_paths.contains(&main_ignored_path));

        std::fs::write(main_path.join(".gitignore"), "ignored_in_main\n\n").unwrap();

        let cached_paths = rescan(
            &mut cache,
            config,
            &temp_dir_path,
            BTreeSet::from([main_path.join(".gitignore")]),
        );

        assert!(
            cached_paths.contains(&worktree_ignored_path),
            "rescanning the main repository must not remove the nested worktree's exclusions"
        );
    }

    #[test]
    #[serial]
    fn test_rescan_adds_and_removes_exclusions_in_the_same_scan() {
        let temp_dir = TempDirectoryBuilder::default().build().unwrap();
        let temp_dir_path = temp_dir.path().canonicalize().unwrap();
        let main_path = temp_dir_path.join("main");

        crate::commands::tests::init_git_repository(&main_path);
        std::fs::write(main_path.join(".gitignore"), "a\nb\n").unwrap();
        commit_all(&main_path, "init main repository");

        std::fs::write(main_path.join("a"), "").unwrap();
        std::fs::write(main_path.join("b"), "").unwrap();

        let mut cache = Cache::open_in_memory().unwrap();
        let config = crate::commands::tests::create_config(&main_path);

        super::super::run::execute(&config, &mut cache, false, false).unwrap();

        let a_path = main_path.join("a");
        let b_path = main_path.join("b");
        let c_path = main_path.join("c");

        let cached_paths: BTreeSet<_> = cache.paths().unwrap().into_iter().collect();
        assert!(cached_paths.contains(&a_path));
        assert!(cached_paths.contains(&b_path));

        std::fs::write(main_path.join(".gitignore"), "b\nc\n").unwrap();
        std::fs::write(&c_path, "").unwrap();

        let cached_paths = rescan(
            &mut cache,
            config,
            &temp_dir_path,
            BTreeSet::from([main_path.join(".gitignore")]),
        );

        assert!(
            !cached_paths.contains(&a_path),
            "'a' is no longer gitignored, it must be removed from the cache"
        );
        assert!(
            cached_paths.contains(&b_path),
            "'b' is still gitignored, it must remain in the cache"
        );
        assert!(
            cached_paths.contains(&c_path),
            "'c' became gitignored, it must be added to the cache"
        );
    }

    #[test]
    #[serial]
    fn test_rescan_main_repository_does_not_remove_submodule_exclusions_when_submodule_directory_is_gitignored()
     {
        let temp_dir = TempDirectoryBuilder::default().build().unwrap();
        let temp_dir_path = temp_dir.path().canonicalize().unwrap();
        let sub_source_path = temp_dir_path.join("sub_source");
        let main_path = temp_dir_path.join("main");

        crate::commands::tests::init_git_repository(&sub_source_path);
        std::fs::write(sub_source_path.join(".gitignore"), "ignored_in_sub\n").unwrap();
        commit_all(&sub_source_path, "init submodule source");

        crate::commands::tests::init_git_repository(&main_path);
        std::fs::write(main_path.join(".gitignore"), "ignored_in_main\n").unwrap();
        commit_all(&main_path, "init main repository");
        run_git(&[
            "-c",
            "protocol.file.allow=always",
            "-C",
            main_path.to_str().unwrap(),
            "submodule",
            "add",
            "-q",
            sub_source_path.to_str().unwrap(),
            "submodule",
        ]);
        commit_all(&main_path, "add submodule");

        std::fs::write(main_path.join("submodule").join("ignored_in_sub"), "").unwrap();
        std::fs::write(main_path.join("ignored_in_main"), "").unwrap();

        let mut cache = Cache::open_in_memory().unwrap();
        let config = crate::commands::tests::create_config(&main_path);

        super::super::run::execute(&config, &mut cache, false, false).unwrap();

        let submodule_ignored_path = main_path.join("submodule").join("ignored_in_sub");
        let main_ignored_path = main_path.join("ignored_in_main");

        let cached_paths: BTreeSet<_> = cache.paths().unwrap().into_iter().collect();
        assert!(
            cached_paths.contains(&submodule_ignored_path),
            "the initial scan should have excluded the submodule's ignored file"
        );
        assert!(cached_paths.contains(&main_ignored_path));

        std::fs::write(
            main_path.join(".gitignore"),
            "ignored_in_main\nsubmodule/\n",
        )
        .unwrap();

        let cached_paths = rescan(
            &mut cache,
            config,
            &temp_dir_path,
            BTreeSet::from([main_path.join(".gitignore")]),
        );

        assert!(
            cached_paths.contains(&submodule_ignored_path),
            "rescanning the main repository must not remove the submodule's exclusions \
             even though the submodule's directory is itself gitignored"
        );
    }

    #[test]
    #[serial]
    fn test_rescan_excludes_a_new_ignored_file_in_a_not_collapsed_directory() {
        let temp_dir = TempDirectoryBuilder::default().build().unwrap();
        let temp_dir_path = temp_dir.path().canonicalize().unwrap();
        let main_path = temp_dir_path.join("main");

        crate::commands::tests::init_git_repository(&main_path);
        std::fs::write(main_path.join(".gitignore"), "*.log\n").unwrap();
        let source_path = main_path.join("src");
        std::fs::create_dir_all(&source_path).unwrap();
        std::fs::write(source_path.join("main.rs"), "").unwrap();
        commit_all(&main_path, "init main repository");

        let mut cache = Cache::open_in_memory().unwrap();
        let config = crate::commands::tests::create_config(&main_path);

        super::super::run::execute(&config, &mut cache, false, false).unwrap();
        assert!(
            cache.paths().unwrap().is_empty(),
            "the repository has nothing ignored yet"
        );

        let new_log_path = source_path.join("new.log");
        std::fs::write(&new_log_path, "").unwrap();

        let cached_paths = rescan(
            &mut cache,
            config,
            &temp_dir_path,
            BTreeSet::from([new_log_path.clone()]),
        );

        assert_eq!(
            cached_paths,
            BTreeSet::from([new_log_path]),
            "'src' holds a tracked file so git does not collapse it: the new ignored file is a \
             new entry of git ls-files and no cached exclusion covers it, so it must be excluded"
        );
    }

    #[test]
    #[serial]
    fn test_rescan_excludes_a_new_wholly_ignored_directory() {
        let temp_dir = TempDirectoryBuilder::default().build().unwrap();
        let temp_dir_path = temp_dir.path().canonicalize().unwrap();
        let main_path = temp_dir_path.join("main");

        crate::commands::tests::init_git_repository(&main_path);
        std::fs::write(main_path.join(".gitignore"), "node_modules/\n").unwrap();
        commit_all(&main_path, "init main repository");

        let mut cache = Cache::open_in_memory().unwrap();
        let config = crate::commands::tests::create_config(&main_path);

        super::super::run::execute(&config, &mut cache, false, false).unwrap();
        assert!(
            cache.paths().unwrap().is_empty(),
            "'node_modules' does not exist yet"
        );

        let node_modules_path = main_path.join("node_modules");
        std::fs::create_dir_all(&node_modules_path).unwrap();
        std::fs::write(node_modules_path.join("a.js"), "").unwrap();
        std::fs::write(node_modules_path.join("b.js"), "").unwrap();

        let cached_paths = rescan(
            &mut cache,
            config,
            &temp_dir_path,
            BTreeSet::from([
                node_modules_path.clone(),
                node_modules_path.join("a.js"),
                node_modules_path.join("b.js"),
            ]),
        );

        assert_eq!(
            cached_paths,
            BTreeSet::from([node_modules_path]),
            "every path an install creates is ignored and exists, but no cached exclusion \
             covers them, so the new directory must be excluded"
        );
    }

    #[test]
    #[serial]
    fn test_rescan_removes_a_directory_exclusion_when_a_non_ignored_file_appears_in_it() {
        let temp_dir = TempDirectoryBuilder::default().build().unwrap();
        let temp_dir_path = temp_dir.path().canonicalize().unwrap();
        let main_path = temp_dir_path.join("main");

        crate::commands::tests::init_git_repository(&main_path);
        std::fs::write(main_path.join(".gitignore"), "*.log\n").unwrap();
        commit_all(&main_path, "init main repository");

        let big_dir_path = main_path.join("big_dir");
        std::fs::create_dir_all(&big_dir_path).unwrap();
        std::fs::write(big_dir_path.join("a.log"), "").unwrap();
        std::fs::write(big_dir_path.join("b.log"), "").unwrap();

        let mut cache = Cache::open_in_memory().unwrap();
        let config = crate::commands::tests::create_config(&main_path);

        super::super::run::execute(&config, &mut cache, false, false).unwrap();

        let cached_paths: BTreeSet<_> = cache.paths().unwrap().into_iter().collect();
        assert!(
            cached_paths.contains(&big_dir_path),
            "the initial scan should have excluded the wholly ignored directory"
        );

        let kept_path = big_dir_path.join("keep.txt");
        std::fs::write(&kept_path, "").unwrap();

        let cached_paths = rescan(
            &mut cache,
            config,
            &temp_dir_path,
            BTreeSet::from([kept_path]),
        );

        assert_eq!(
            cached_paths,
            BTreeSet::from([big_dir_path.join("a.log"), big_dir_path.join("b.log")]),
            "the directory is no longer wholly ignored so git stopped collapsing it: its \
             exclusion must be removed, otherwise the new file is left out of the backup, \
             and the exclusions of the ignored files it contains must be kept"
        );
    }

    #[test]
    #[serial]
    fn test_rescan_main_repository_removes_the_exclusion_it_created_for_a_nested_repository() {
        let temp_dir = TempDirectoryBuilder::default().build().unwrap();
        let temp_dir_path = temp_dir.path().canonicalize().unwrap();
        let main_path = temp_dir_path.join("main");

        crate::commands::tests::init_git_repository(&main_path);
        std::fs::write(main_path.join(".gitignore"), "ignored_in_main\n").unwrap();
        commit_all(&main_path, "init main repository");

        let nested_path = main_path.join("nested");
        crate::commands::tests::init_git_repository(&nested_path);
        std::fs::write(nested_path.join(".gitignore"), "ignored_in_nested\n").unwrap();
        commit_all(&nested_path, "init nested repository");

        std::fs::write(nested_path.join("ignored_in_nested"), "").unwrap();
        std::fs::write(main_path.join("ignored_in_main"), "").unwrap();

        let main_ignored_path = main_path.join("ignored_in_main");
        let nested_ignored_path = nested_path.join("ignored_in_nested");

        let mut cache = Cache::open_in_memory().unwrap();
        let config = crate::commands::tests::create_config(&main_path);

        super::super::run::execute(&config, &mut cache, false, false).unwrap();

        let cached_paths: BTreeSet<_> = cache.paths().unwrap().into_iter().collect();
        assert_eq!(
            cached_paths,
            BTreeSet::from([main_ignored_path.clone(), nested_ignored_path.clone()]),
            "the initial scan should have excluded the ignored file of each repository"
        );

        std::fs::write(main_path.join(".gitignore"), "ignored_in_main\nnested/\n").unwrap();

        let cached_paths = rescan(
            &mut cache,
            config,
            &temp_dir_path,
            BTreeSet::from([main_path.join(".gitignore")]),
        );

        assert_eq!(
            cached_paths,
            BTreeSet::from([
                main_ignored_path.clone(),
                nested_path.clone(),
                nested_ignored_path.clone()
            ]),
            "the main repository now gitignores the nested repository, so it must exclude it \
             without dropping the exclusion owned by the nested repository"
        );

        std::fs::write(main_path.join(".gitignore"), "ignored_in_main\n").unwrap();

        let cached_paths = rescan(
            &mut cache,
            crate::commands::tests::create_config(&main_path),
            &temp_dir_path,
            BTreeSet::from([main_path.join(".gitignore")]),
        );

        assert_eq!(
            cached_paths,
            BTreeSet::from([main_ignored_path, nested_ignored_path]),
            "the main repository created the exclusion of the nested repository and no longer \
             gitignores it, so rescanning the main repository must remove it and keep the \
             exclusion owned by the nested repository"
        );
    }

    #[test]
    #[serial]
    fn test_rescan_removes_an_exclusion_after_a_repository_appears_under_it() {
        let temp_dir = TempDirectoryBuilder::default().build().unwrap();
        let temp_dir_path = temp_dir.path().canonicalize().unwrap();
        let main_path = temp_dir_path.join("main");

        crate::commands::tests::init_git_repository(&main_path);
        std::fs::write(main_path.join(".gitignore"), "vendor/thing\n").unwrap();
        let thing_path = main_path.join("vendor").join("thing");
        std::fs::create_dir_all(&thing_path).unwrap();
        std::fs::write(thing_path.join("file"), "").unwrap();
        commit_all(&main_path, "init main repository");

        let mut cache = Cache::open_in_memory().unwrap();
        let config = crate::commands::tests::create_config(&main_path);

        super::super::run::execute(&config, &mut cache, false, false).unwrap();

        let cached_paths: BTreeSet<_> = cache.paths().unwrap().into_iter().collect();
        assert!(
            cached_paths.contains(&thing_path),
            "the initial scan should have excluded the ignored directory"
        );

        crate::commands::tests::init_git_repository(main_path.join("vendor"));
        std::fs::write(main_path.join(".gitignore"), "\n").unwrap();

        let cached_paths = rescan(
            &mut cache,
            config,
            &temp_dir_path,
            BTreeSet::from([main_path.join(".gitignore")]),
        );

        assert!(
            !cached_paths.contains(&thing_path),
            "the main repository created this exclusion and no longer gitignores it, so \
             rescanning the main repository must remove it even though a repository has since \
             appeared between it and the main repository"
        );
    }

    #[test]
    #[serial]
    fn test_a_cache_written_before_the_repository_was_recorded_is_usable_after_a_full_scan() {
        let temp_dir = TempDirectoryBuilder::default().build().unwrap();
        let temp_dir_path = temp_dir.path().canonicalize().unwrap();
        let main_path = temp_dir_path.join("main");

        crate::commands::tests::init_git_repository(&main_path);
        std::fs::write(main_path.join(".gitignore"), "a\nbig_dir\n").unwrap();
        commit_all(&main_path, "init main repository");

        let a_path = main_path.join("a");
        let big_dir_path = main_path.join("big_dir");
        std::fs::write(&a_path, "").unwrap();
        std::fs::create_dir_all(&big_dir_path).unwrap();
        std::fs::write(big_dir_path.join("b"), "").unwrap();

        let cache_file_path = temp_dir_path.join("cache.db");
        crate::cache::tests::write_version_1_cache(
            &cache_file_path,
            &[
                a_path.clone(),
                PathBuf::from(format!("{}/", big_dir_path.display())),
            ],
        );
        let mut cache = Cache::open(&cache_file_path).unwrap();
        let config = crate::commands::tests::create_config(&main_path);

        assert!(
            cache.paths_created_by(&main_path).unwrap().is_empty(),
            "the migrated exclusions have no owner yet"
        );

        super::super::run::execute(&config, &mut cache, false, false).unwrap();

        let mut owned = cache.paths_created_by(&main_path).unwrap();
        owned.sort_unstable();
        assert_eq!(
            vec![a_path.clone(), big_dir_path.clone()],
            owned,
            "the full scan must re-attribute every exclusion to the repository that produced it"
        );

        std::fs::write(main_path.join(".gitignore"), "big_dir\n").unwrap();

        let cached_paths = rescan(
            &mut cache,
            config,
            &temp_dir_path,
            BTreeSet::from([main_path.join(".gitignore")]),
        );

        assert_eq!(
            BTreeSet::from([big_dir_path]),
            cached_paths,
            "'a' is no longer gitignored so the rescan must remove it, which it can only do now \
             that the migrated rows carry an owner"
        );
    }

    #[test]
    #[serial]
    fn test_rescan_large_ignored_tree_preserves_exclusions() {
        let temp_dir = TempDirectoryBuilder::default().build().unwrap();
        let temp_dir_path = temp_dir.path().canonicalize().unwrap();
        let main_path = temp_dir_path.join("main");

        crate::commands::tests::init_git_repository(&main_path);
        std::fs::write(main_path.join(".gitignore"), "*.ignored\n").unwrap();
        commit_all(&main_path, "init main repository");

        let mut ignored_paths = BTreeSet::new();
        for i in 0..1000 {
            let file_path = main_path.join(format!("file_{i}.ignored"));
            std::fs::write(&file_path, "").unwrap();
            ignored_paths.insert(file_path);
        }

        let mut cache = Cache::open_in_memory().unwrap();
        let config = crate::commands::tests::create_config(&main_path);

        super::super::run::execute(&config, &mut cache, false, false).unwrap();

        let cached_paths: BTreeSet<_> = cache.paths().unwrap().into_iter().collect();
        for path in &ignored_paths {
            assert!(
                cached_paths.contains(path),
                "the initial scan should have excluded every ignored file"
            );
        }
        assert_eq!(cached_paths.len(), ignored_paths.len());

        std::fs::write(main_path.join(".gitignore"), "*.ignored\n\n").unwrap();

        let cached_paths = rescan(
            &mut cache,
            config,
            &temp_dir_path,
            BTreeSet::from([main_path.join(".gitignore")]),
        );

        for path in &ignored_paths {
            assert!(
                cached_paths.contains(path),
                "rescanning must not drop cached exclusions when many paths are cached"
            );
        }
        assert_eq!(cached_paths.len(), ignored_paths.len());
    }

    #[test]
    #[serial]
    #[ignore = "large-scale scenario kept for stress-testing and manual profiling; run explicitly with `cargo test -- --ignored`"]
    fn test_rescan_large_ignored_tree_preserves_exclusions_at_scale() {
        let temp_dir = TempDirectoryBuilder::default().build().unwrap();
        let temp_dir_path = temp_dir.path().canonicalize().unwrap();
        let main_path = temp_dir_path.join("main");

        crate::commands::tests::init_git_repository(&main_path);
        std::fs::write(main_path.join(".gitignore"), "*.ignored\n").unwrap();
        commit_all(&main_path, "init main repository");

        let deep_path = main_path.join("a").join("b").join("c");
        std::fs::create_dir_all(&deep_path).unwrap();
        std::fs::write(deep_path.join(".keep"), "").unwrap();
        commit_all(&main_path, "add deep sentinel");

        let iterations = test_iterations(100_000);

        let mut ignored_paths = BTreeSet::new();
        for i in 0..iterations {
            let file_path = deep_path.join(format!("file_{i}.ignored"));
            std::fs::write(&file_path, "").unwrap();
            ignored_paths.insert(file_path);
        }

        let mut cache = Cache::open_in_memory().unwrap();
        let config = crate::commands::tests::create_config(&main_path);

        super::super::run::execute(&config, &mut cache, false, false).unwrap();

        let cached_paths: BTreeSet<_> = cache.paths().unwrap().into_iter().collect();
        for path in &ignored_paths {
            assert!(
                cached_paths.contains(path),
                "the initial scan should have excluded every ignored file"
            );
        }
        assert_eq!(cached_paths.len(), ignored_paths.len());

        std::fs::write(main_path.join(".gitignore"), "*.ignored\n\n").unwrap();

        let cached_paths = rescan(
            &mut cache,
            config,
            &temp_dir_path,
            BTreeSet::from([main_path.join(".gitignore")]),
        );

        for path in &ignored_paths {
            assert!(
                cached_paths.contains(path),
                "rescanning must not drop cached exclusions under a large ignored directory"
            );
        }
        assert_eq!(cached_paths.len(), ignored_paths.len());
    }

    #[test]
    #[serial]
    #[ignore = "kept for stress-testing and manual profiling; run explicitly with `cargo test -- --ignored`"]
    fn test_rescan_huge_ignored_tree_with_no_nested_repositories() {
        let temp_dir = TempDirectoryBuilder::default().build().unwrap();
        let temp_dir_path = temp_dir.path().canonicalize().unwrap();
        let main_path = temp_dir_path.join("main");

        crate::commands::tests::init_git_repository(&main_path);
        std::fs::write(main_path.join(".gitignore"), "big_dir\n").unwrap();
        commit_all(&main_path, "init main repository");

        let big_dir = main_path.join("big_dir");
        std::fs::create_dir_all(&big_dir).unwrap();
        let iterations = test_iterations(200_000);
        for i in 0..iterations {
            std::fs::write(big_dir.join(format!("file_{i}")), "").unwrap();
        }

        let mut cache = Cache::open_in_memory().unwrap();
        let config = crate::commands::tests::create_config(&main_path);

        super::super::run::execute(&config, &mut cache, false, false).unwrap();

        let cached_paths: BTreeSet<_> = cache.paths().unwrap().into_iter().collect();
        assert_eq!(
            cached_paths,
            BTreeSet::from([big_dir.clone()]),
            "git ls-files --directory collapses a wholly-ignored directory into one entry"
        );

        std::fs::write(main_path.join(".gitignore"), "big_dir\n\n").unwrap();

        let cached_paths = rescan(
            &mut cache,
            config,
            &temp_dir_path,
            BTreeSet::from([main_path.join(".gitignore")]),
        );

        assert_eq!(cached_paths, BTreeSet::from([big_dir]));
    }

    #[test]
    fn test_find_repositories_to_scan() {
        let temp_dir = TempDirectoryBuilder::default().build().unwrap();
        let temp_dir_path = temp_dir.path().canonicalize().unwrap();
        let repository_path = temp_dir_path.join("repository");
        let outside_path = temp_dir_path.join("outside");

        crate::commands::tests::init_git_repository(&repository_path);
        crate::commands::tests::init_git_repository(&outside_path);
        std::fs::write(repository_path.join(".gitignore"), "target\n*.log\n").unwrap();

        let target_path = repository_path.join("target").join("debug");
        std::fs::create_dir_all(&target_path).unwrap();
        std::fs::write(target_path.join("binary"), "").unwrap();

        let logs_path = repository_path.join("logs");
        std::fs::create_dir_all(&logs_path).unwrap();
        std::fs::write(logs_path.join("a.log"), "").unwrap();

        let kept_path = logs_path.join("keep.txt");
        std::fs::write(&kept_path, "").unwrap();

        let source_path = repository_path.join("src");
        std::fs::create_dir_all(&source_path).unwrap();
        std::fs::write(source_path.join("main.rs"), "").unwrap();

        let new_log_path = repository_path.join("new.log");
        std::fs::write(&new_log_path, "").unwrap();

        std::fs::write(outside_path.join("file"), "").unwrap();

        let search_directories = BTreeSet::from([repository_path.clone()]);
        let mut cache = Cache::open_in_memory().unwrap();
        cache
            .reset(
                [repository_path.join("target"), repository_path.join("logs")]
                    .map(|path| crate::diff::Exclusion::new(path, repository_path.clone())),
            )
            .unwrap();

        let scan = |paths: [PathBuf; 1]| {
            super::find_repositories_to_scan(&BTreeSet::from(paths), &search_directories, &cache)
                .unwrap()
        };
        let scanned = BTreeSet::from([repository_path.clone()]);

        assert_eq!(scanned, scan([source_path.join("main.rs")]));

        assert!(
            scan([target_path.join("binary")]).is_empty(),
            "an ignored path a cached exclusion already covers cannot change what the scan of \
             the repository reports"
        );

        assert!(
            scan([logs_path.join("a.log")]).is_empty(),
            "a directory wholly ignored stays collapsed when another ignored file changes in it"
        );

        assert_eq!(
            scanned,
            scan([new_log_path]),
            "no cached exclusion covers this ignored path, so it is a new entry of git ls-files \
             and a new exclusion to add"
        );

        assert_eq!(
            scanned,
            scan([kept_path.clone()]),
            "a path that is not ignored stops the collapsing of its directory, so the exclusion \
             of that directory must be recomputed"
        );

        assert!(
            scan([target_path.join("deleted_binary")]).is_empty(),
            "the exclusion covering this path is still there, so deleting a path under it cannot \
             change what the scan of the repository reports"
        );

        assert_eq!(
            scanned,
            scan([repository_path.join("target")]),
            "the deleted path is itself the cached exclusion: nothing covers it any more, so it \
             is an exclusion to remove"
        );

        assert!(
            scan([outside_path.join("file")]).is_empty(),
            "a repository outside the search directories must not be scanned"
        );

        let repositories = super::find_repositories_to_scan(
            &BTreeSet::from([logs_path.join("a.log"), kept_path]),
            &search_directories,
            &cache,
        )
        .unwrap();
        assert_eq!(
            scanned, repositories,
            "a single path that is not ignored is enough to scan the repository"
        );
    }

    #[rstest]
    #[case(Ok(true), true)]
    #[case(Ok(false), false)]
    #[case(Err(anyhow::anyhow!("can't determine")), false)]
    fn test_log_status(#[case] status: anyhow::Result<bool>, #[case] expected: bool) {
        assert_eq!(expected, super::log_status(status));
    }

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
        let thread_handle = std::thread::spawn(move || {
            let mut cache = Cache::open_in_memory().unwrap();

            super::execute(&config_file_path, None, &mut cache, false, true).unwrap();

            cache
        });
        // Need to wait to ensure the internal Monitor is created by super::execute() to ensure
        // the signal will be handled.
        std::thread::sleep(Duration::from_secs(5));
        crate::commands::tests::send_sigint();
        let cache = thread_handle.join().unwrap();
        let paths = cache.paths().unwrap();
        assert_eq!(paths.len(), 3);
    }
}
