mod cache;
mod config;
mod diff;
mod git;
mod legacy_cache;
mod timemachine;

use clap::{Parser, Subcommand};
use regex::RegexSet;
use std::{collections::BTreeSet, error::Error, path::Path};

use crate::{
    cache::{Cache, OpenOrCreate, OpenOrCreateError},
    config::Config,
    git::FindIgnoredFileError,
    legacy_cache::LegacyCache,
};

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Scan for paths to add or remove from the backup exclusion list
    Run {
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        details: bool,
    },
    /// Print the backup exclusion list
    List,
    /// Reset the backup exclusion list
    Reset {
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        details: bool,
    },
    /// Monitor for changes and update the backup exclusion list periodically
    Monitor {
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        details: bool,
    },
}

fn main() -> Result<(), Box<dyn Error>> {
    const CONFIG_FILE_PATH: &str = "~/.config/tmignore/config.json";

    let cli = Cli::parse();
    let config_file_path = shellexpand::tilde(CONFIG_FILE_PATH).to_string();
    let mut cache = open_cache()?;

    match cli.command {
        Commands::Run { dry_run, details } => {
            let mut logger = Logger::new(dry_run);
            let config = Config::load_or_create_file(&config_file_path)?;

            run_command::execute(&config, &mut cache, dry_run, details, &mut logger)
        }
        Commands::List => list_command::execute(cache),
        Commands::Reset { dry_run, details } => {
            let mut logger = Logger::new(dry_run);

            reset_command::execute(cache, dry_run, details, &mut logger)
        }
        Commands::Monitor { dry_run, details } => {
            let mut logger = Logger::new(dry_run);

            monitor_command::execute(&config_file_path, &mut cache, dry_run, details, &mut logger)
        }
    }?;

    Ok(())
}

#[derive(thiserror::Error, Debug)]
enum OpenCacheError {
    #[error("No cache directory")]
    NoCacheDirectory,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    OpenOrCreate(#[from] OpenOrCreateError),
}

fn open_cache() -> Result<Cache, OpenCacheError> {
    // ~/Library/Caches/tmignore-rs/cache.db
    let cache_file_path = dirs::cache_dir()
        .ok_or(OpenCacheError::NoCacheDirectory)?
        .join("tmignore-rs")
        .join("cache.db");
    std::fs::create_dir_all(
        cache_file_path
            .parent()
            .ok_or(OpenCacheError::NoCacheDirectory)?,
    )?;

    Ok(match Cache::open_or_create(cache_file_path)? {
        OpenOrCreate::Created(mut cache) => {
            let paths_to_import = LegacyCache::import()?;
            cache.reset(paths_to_import);
            cache
        }
        OpenOrCreate::Opened(cache) => cache,
    })
}

struct ApplyError<'a> {
    error: std::io::Error,
    path: &'a Path,
    added: bool,
}

fn apply_diff_and_print<'a>(
    diff: &'a crate::diff::Diff,
    dry_run: bool,
    details: bool,
    logger: &mut Logger,
) -> Vec<&'a Path> {
    let mut errors = Vec::new();
    let mut add_failed_paths = BTreeSet::new();

    for path in &diff.added {
        if !dry_run && let Err(error) = timemachine::add_exclusion(path) {
            add_failed_paths.insert(path);
            errors.push(ApplyError {
                error,
                path,
                added: true,
            });
        }
    }

    for path in &diff.removed {
        if !dry_run
            && path.exists()
            && let Err(error) = timemachine::remove_exclusion(path)
        {
            errors.push(ApplyError {
                error,
                path,
                added: false,
            });
        }
    }

    let add_count = diff.added.len() - add_failed_paths.len();
    let remove_count = diff.removed.len();

    if add_count > 0 {
        logger.log(format!(
            "Added {} paths to the backup exclusion list",
            add_count
        ));
    }

    if remove_count > 0 {
        logger.log(format!(
            "Removed {} paths from the backup exclusion list",
            remove_count
        ));
    }

    if add_count == 0 && remove_count == 0 {
        logger.log("No changes to the backup exclusion list");
    }

    if details {
        for path in &diff.added {
            if !add_failed_paths.contains(path) {
                logger.log(format!("+ {}", path.display()));
            }
        }
    }

    if details {
        for path in &diff.removed {
            logger.log(format!("- {}", path.display()));
        }
    }

    for error in &errors {
        eprintln!("Error: {}: {}", error.path.display(), error.error);
    }

    errors
        .into_iter()
        .filter(|error| error.added)
        .map(|entry| entry.path)
        .collect()
}

struct Logger {
    dry_run: bool,
}

impl Logger {
    pub fn new(dry_run: bool) -> Self {
        Self { dry_run }
    }

    pub fn log(&mut self, str: impl AsRef<str>) {
        if self.dry_run {
            println!("[DRY RUN] {}", str.as_ref());
        } else {
            println!("{}", str.as_ref());
        }
    }
}

fn create_whitelist(whitelist_patterns: &BTreeSet<String>) -> Result<RegexSet, regex::Error> {
    RegexSet::new(whitelist_patterns.iter().filter_map(|pattern| {
        match fnmatch_regex::glob_to_regex_pattern(pattern) {
            Ok(pattern) => Some(pattern),
            Err(error) => {
                eprintln!("Error: invalid whitelist pattern '{}': {}", pattern, error);
                None
            }
        }
    }))
}

/// Find the paths in a repository to exclude from Time Machine backup.
/// If a path matches at least one of the regexes in the `whitelist` RegexSet it will not be
/// added to the `exclusion` set.
fn find_paths_to_exclude_from_backup(
    repository_path: impl AsRef<Path>,
    whitelist: &RegexSet,
    exclusions: &mut BTreeSet<std::path::PathBuf>,
) -> Result<(), FindIgnoredFileError> {
    let repository_path = repository_path.as_ref();
    let ignored_files = git::find_ignored_files(repository_path)?;

    for ignored_file in ignored_files {
        if let Some(ignored_file) = ignored_file.to_str()
            && whitelist.is_match(ignored_file)
        {
            continue;
        }
        exclusions.insert(ignored_file);
    }

    Ok(())
}

mod run_command {
    use std::{collections::BTreeSet, error::Error};

    use crate::{
        Logger,
        cache::Cache,
        config::Config,
        create_whitelist, find_paths_to_exclude_from_backup,
        git::{self},
    };

    pub fn execute(
        config: &Config,
        cache: &mut Cache,
        dry_run: bool,
        details: bool,
        logger: &mut Logger,
    ) -> Result<(), Box<dyn Error>> {
        let whitelist = create_whitelist(&config.whitelist_patterns)?;
        let mut repositories = BTreeSet::new();
        let mut exclusions = BTreeSet::new();

        logger.log("Searching for Git repositories...");
        if let Some((rx, thread_handle)) = git::find_repositories(
            &config.search_directories,
            &config.ignored_directories,
            config.threads.unwrap_or_default(),
        ) {
            while let Ok(repository_path) = rx.recv() {
                repositories.insert(repository_path.clone());

                find_paths_to_exclude_from_backup(repository_path, &whitelist, &mut exclusions)?;
            }
            thread_handle.join().unwrap();

            logger.log(format!("Found {} repositories", repositories.len()));

            let diff = cache.find_diff(&exclusions);

            let paths_failed_to_add = super::apply_diff_and_print(&diff, dry_run, details, logger);

            for path in paths_failed_to_add {
                exclusions.remove(path);
            }

            if !dry_run {
                cache.reset(exclusions);
            }
        }

        Ok(())
    }
}

mod list_command {
    use std::error::Error;

    use crate::cache::Cache;

    pub fn execute(cache: Cache) -> Result<(), Box<dyn Error>> {
        for path in cache.paths() {
            println!("{}", path.display());
        }
        Ok(())
    }
}

mod reset_command {
    use crate::{Logger, cache::Cache};

    use std::{collections::BTreeSet, error::Error};

    pub fn execute(
        mut cache: Cache,
        dry_run: bool,
        details: bool,
        logger: &mut Logger,
    ) -> Result<(), Box<dyn Error>> {
        let diff = cache.find_diff(&BTreeSet::new());

        super::apply_diff_and_print(&diff, dry_run, details, logger);

        if !dry_run {
            cache.reset([]);
        }

        Ok(())
    }
}

mod monitor_command {
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
        Logger, apply_diff_and_print, cache::Cache, config::Config, create_whitelist,
        find_paths_to_exclude_from_backup, git,
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
        let mut whitelist = create_whitelist(&config.whitelist_patterns)?;

        crate::run_command::execute(&config, cache, dry_run, details, logger)?;

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
                            whitelist = create_whitelist(&config.whitelist_patterns)?;
                            println!("Configuration reloaded");
                            crate::run_command::execute(&config, cache, dry_run, details, logger)?;
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
                    find_paths_to_exclude_from_backup(
                        repository_to_scan,
                        &whitelist,
                        &mut exclusions,
                    )?;
                    let diff = cache.find_diff_in_directory(&exclusions, repository_to_scan);
                    let paths_failed_to_add = apply_diff_and_print(&diff, dry_run, details, logger);

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
}
