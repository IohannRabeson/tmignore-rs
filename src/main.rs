mod cache;
mod config;
mod diff;
mod git;
mod legacy_cache;
mod timemachine;

use clap::{Parser, Subcommand};
use std::{collections::BTreeSet, error::Error, path::Path};

use crate::{
    cache::{Cache, OpenOrCreate, OpenOrCreateError},
    config::Config,
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
    // ~/Library/Caches/tmignore-rs/cache.json
    let cache_file_path = dirs::cache_dir()
        .ok_or(OpenCacheError::NoCacheDirectory)?
        .join("tmignore-rs")
        .join("cache.json");
    std::fs::create_dir_all(
        cache_file_path
            .parent()
            .ok_or(OpenCacheError::NoCacheDirectory)?,
    )?;

    Ok(match Cache::open_or_create(cache_file_path)? {
        OpenOrCreate::Created(mut cache) => {
            let paths_to_import = LegacyCache::import()?;
            cache.write(paths_to_import);
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
        if !dry_run && let Err(error) = timemachine::remove_exclusion(path) {
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
        logger.log(format!("No changes to the backup exclusion list"));
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

mod run_command {
    use std::{collections::BTreeSet, error::Error};

    use regex::RegexSet;

    use crate::{Logger, cache::Cache, config::Config, git};

    pub fn execute(
        config: &Config,
        cache: &mut Cache,
        dry_run: bool,
        details: bool,
        mut logger: &mut Logger,
    ) -> Result<(), Box<dyn Error>> {
        let whitelist = RegexSet::new(config.whitelist_patterns.iter().filter_map(|pattern| {
            match fnmatch_regex::glob_to_regex_pattern(pattern) {
                Ok(pattern) => Some(pattern),
                Err(error) => {
                    eprintln!("Error: invalid whitelist pattern '{}': {}", pattern, error);
                    None
                }
            }
        }))?;
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

                let ignored_files = git::find_ignored_files(&repository_path);

                for ignored_file in ignored_files {
                    if let Some(ignored_file) = ignored_file.to_str()
                        && whitelist.is_match(ignored_file)
                    {
                        continue;
                    }
                    exclusions.insert(ignored_file);
                }
            }
            thread_handle.join().unwrap();

            logger.log(format!("Found {} repositories", repositories.len()));

            let diff = cache.find_diff(&exclusions);

            let paths_failed_to_add =
                super::apply_diff_and_print(&diff, dry_run, details, &mut logger);

            for path in paths_failed_to_add {
                exclusions.remove(path);
            }

            if !dry_run {
                cache.write(exclusions);
                cache.save_to_file()?;
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
            cache.write([]);
            cache.save_to_file()?;
        }

        Ok(())
    }
}

mod monitor_command {
    use std::{
        error::Error,
        path::{Path, PathBuf},
        sync::{Arc, atomic::AtomicBool},
        time::{Duration, Instant},
    };

    use crossbeam_channel::Sender;
    use notify::Watcher;

    use crate::{Logger, cache::Cache, config::Config};

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
        let run_interval = Duration::from_secs(5);
        let mut elapsed = Duration::ZERO;
        let mut now = Instant::now();
        let mut need_to_run = false;

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
                            println!("Configuration reloaded");
                        }

                        if filter_event(&config, &event) {
                            need_to_run = true;
                        }
                    }
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => (),
                Err(error) => return Err(Box::new(error)),
            }

            elapsed += Instant::now() - now;
            now = Instant::now();

            if need_to_run && elapsed >= run_interval {
                need_to_run = false;
                elapsed = Duration::ZERO;
                crate::run_command::execute(&config, cache, dry_run, details, logger)?;
            }
        }
        logger.log("Monitor stopped");
        Ok(())
    }

    fn filter_event(config: &Config, event: &notify::Event) -> bool {
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
