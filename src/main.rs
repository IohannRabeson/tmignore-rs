mod cache;
mod commands;
mod config;
mod diff;
mod git;
mod legacy_cache;
mod timemachine;

use clap::{Parser, Subcommand};
use std::{error::Error, path::Path};

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
    const CACHE_FILE_PATH: &str = "~/Library/Caches/tmignore-rs/cache.db";
    const LEGACY_CACHE_FILE_PATH: &str = "~/Library/Caches/tmignore/cache.json";

    let cli = Cli::parse();
    let config_file_path = shellexpand::tilde(CONFIG_FILE_PATH).to_string();
    let cache_file_path = shellexpand::tilde(CACHE_FILE_PATH).to_string();
    let legacy_cache_file_path = shellexpand::tilde(LEGACY_CACHE_FILE_PATH).to_string();
    let mut cache = open_cache(cache_file_path, legacy_cache_file_path)?;

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

fn open_cache(
    cache_file_path: impl AsRef<Path>,
    legacy_cache_file_path: impl AsRef<Path>,
) -> Result<Cache, OpenCacheError> {
    let cache_file_path = cache_file_path.as_ref();

    std::fs::create_dir_all(
        cache_file_path
            .parent()
            .ok_or(OpenCacheError::NoCacheDirectory)?,
    )?;

    Ok(match Cache::open_or_create(cache_file_path)? {
        OpenOrCreate::Created(mut cache) => {
            let paths_to_import = LegacyCache::import(legacy_cache_file_path)?;
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use temp_dir_builder::TempDirectoryBuilder;

    use crate::{OpenCacheError, open_cache};

    #[test]
    fn test_open_cache_no_parent_dir() {
        let result = open_cache("/", "dummy");

        assert!(matches!(result, Err(OpenCacheError::NoCacheDirectory)));
    }

    #[test]
    fn test_open_cache_create_no_legacy() {
        let temp_dir = TempDirectoryBuilder::default().build().unwrap();
        let cache_file_path = temp_dir.path().join("cache.db");
        let result = open_cache(cache_file_path, temp_dir.path().join("doesnotexist")).unwrap();

        assert!(result.paths().is_empty());
    }

    #[test]
    fn test_open_cache_create_legacy() {
        let cache_content = r#"{"paths":["yo"]}"#;
        let legacy_cache_name = "legacy.json";
        let temp_dir = TempDirectoryBuilder::default()
            .add_text_file(legacy_cache_name, cache_content)
            .build()
            .unwrap();
        let cache_file_path = temp_dir.path().join("cache.db");
        let legacy_file_path = temp_dir.path().join(legacy_cache_name);
        let result = open_cache(cache_file_path, &legacy_file_path).unwrap();
        let paths = result.paths();
        assert_eq!(1, paths.len());
        assert_eq!(PathBuf::from("yo"), paths[0]);
    }

    #[test]
    fn test_open_cache_existing() {
        let temp_dir = TempDirectoryBuilder::default().build().unwrap();
        let cache_file_path = temp_dir.path().join("cache.db");
        let legacy_cache_path = temp_dir.path().join("dummy");
        {
            let mut cache = open_cache(&cache_file_path, &legacy_cache_path).unwrap();
            cache.add_paths([PathBuf::from("yo")].into_iter());
        }
        let cache = open_cache(&cache_file_path, &legacy_cache_path).unwrap();
        let paths = cache.paths();
        assert_eq!(1, paths.len());
        assert_eq!(PathBuf::from("yo"), paths[0]);
    }
}
