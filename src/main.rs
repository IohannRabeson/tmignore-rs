mod cache;
mod commands;
mod config;
mod diff;
mod git;
mod legacy_cache;
mod timemachine;

use clap::{Parser, Subcommand};
use regex::RegexSet;
use std::{
    collections::BTreeSet,
    error::Error,
    path::{Path, PathBuf},
};

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
    /// Watch for file changes and keep the exclusion list up to date
    ///
    /// Begins with a complete scan to ensure the exclusion list is up to date.
    /// If the configuration file is modified, it is reloaded and a
    /// complete scan is triggered.
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

            commands::run::execute(&config, &mut cache, dry_run, details, &mut logger)
        }
        Commands::List => commands::list::execute(cache),
        Commands::Reset { dry_run, details } => {
            let mut logger = Logger::new(dry_run);

            commands::reset::execute(&mut cache, dry_run, details, &mut logger)
        }
        Commands::Monitor { dry_run, details } => {
            let mut logger = Logger::new(dry_run);

            commands::monitor::execute(&config_file_path, &mut cache, dry_run, details, &mut logger)
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
    let cache_file_path =
        PathBuf::from(shellexpand::tilde("~/Library/Caches/tmignore-rs/cache.db").to_string());
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
