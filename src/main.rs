mod cache;
mod commands;
mod config;
mod diff;
mod git;
mod json;
mod legacy_cache;
mod legacy_config;
mod timemachine;

use clap::{Parser, Subcommand};
use log::{error, info};
use std::{error::Error, path::Path};

use crate::{cache::Cache, commands::monitor::Monitor, config::Config, legacy_cache::LegacyCache};

#[derive(Parser)]
#[command(about, long_about = None)]
#[command(version = concat!(env!("CARGO_PKG_VERSION"), "-", env!("VERGEN_GIT_SHA")))]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
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
    /// Scan for paths to add or remove from the backup exclusion list
    Run {
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        details: bool,
    },
    /// Print the backup exclusion list
    List {
        #[arg(short = '0', long)]
        zero_separator: bool,
    },
    /// Reset the backup exclusion list
    Reset {
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        details: bool,
    },
}

const CONFIG_FILE_PATH: &str = "~/.config/tmignore-rs/config.json";
const CACHE_FILE_PATH: &str = "~/Library/Caches/tmignore-rs/cache.db";
const LEGACY_CONFIG_FILE_PATH: &str = "~/.config/tmignore/config.json";
const LEGACY_CACHE_FILE_PATH: &str = "~/Library/Caches/tmignore/cache.json";

fn program() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();
    let config_file_path = shellexpand::tilde(CONFIG_FILE_PATH).to_string();
    let cache_file_path = shellexpand::tilde(CACHE_FILE_PATH).to_string();
    let legacy_config_file_path = shellexpand::tilde(LEGACY_CONFIG_FILE_PATH).to_string();
    let legacy_cache_file_path = shellexpand::tilde(LEGACY_CACHE_FILE_PATH).to_string();

    setup_log()?;
    import_legacy_config_file(&legacy_config_file_path, &config_file_path)?;
    import_legacy_cache_file(&legacy_cache_file_path, &cache_file_path)?;

    match cli.command {
        Commands::Run { dry_run, details } => {
            let mut logger = Logger::new(dry_run);
            let mut cache = Cache::open(cache_file_path)?;
            let config = Config::load_or_create_file(&config_file_path)?;

            commands::run::execute(&config, &mut cache, dry_run, details, &mut logger)
        }
        Commands::List { zero_separator } => {
            let cache = Cache::open(cache_file_path)?;
            let separator = if zero_separator { '\0' } else { '\n' };

            commands::list::execute(cache, &mut std::io::stdout(), separator)
        }
        Commands::Reset { dry_run, details } => {
            let mut logger = Logger::new(dry_run);
            let mut cache = Cache::open(cache_file_path)?;

            commands::reset::execute(&mut cache, dry_run, details, &mut logger)
        }
        Commands::Monitor { dry_run, details } => {
            let mut logger = Logger::new(dry_run);
            let mut cache = Cache::open(cache_file_path)?;
            let mut monitor = Monitor::new(&config_file_path)?;

            commands::monitor::execute(
                &config_file_path,
                &mut cache,
                dry_run,
                details,
                &mut logger,
                &mut monitor,
            )
        }
    }?;

    Ok(())
}

fn setup_log() -> Result<(), log::SetLoggerError> {
    let level = log::LevelFilter::Info;
    let os_logger: Box<dyn log::Log> =
        Box::new(oslog::OsLogger::new("com.irabeson.tmignore-rs"));

    fern::Dispatch::new()
        .level(level)
        .chain(std::io::stdout())
        .chain(os_logger)
        .apply()?;
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    match program() {
        Ok(()) => Ok(()),
        Err(error) => {
            error!("Error: {}", error);
            eprintln!("Error: {}", error);
            Err(error)
        }
    }
}

fn import_legacy_config_file(
    legacy_config_file_path: impl AsRef<Path>,
    config_file_path: impl AsRef<Path>,
) -> Result<(), json::Error> {
    let legacy_config_file_path = legacy_config_file_path.as_ref();
    let config_file_path = config_file_path.as_ref();
    if !legacy_config_file_path.is_file() || config_file_path.is_file() {
        return Ok(());
    }
    info!(
        "Importing legacy config '{}'...",
        legacy_config_file_path.display()
    );
    let legacy_config = json::load_json_file(&legacy_config_file_path)?;
    let new_config = Config::from(&legacy_config);
    if let Some(parent) = config_file_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    json::save_json_file(&config_file_path, &new_config)?;
    info!("Create new config file '{}'", config_file_path.display());
    info!("You can delete '{}' now", legacy_config_file_path.display());
    Ok(())
}

fn import_legacy_cache_file(
    legacy_cache_file_path: impl AsRef<Path>,
    cache_file_path: impl AsRef<Path>,
) -> Result<(), Box<dyn Error>> {
    let legacy_cache_file_path = legacy_cache_file_path.as_ref();
    let cache_file_path = cache_file_path.as_ref();

    if !legacy_cache_file_path.is_file() || cache_file_path.is_file() {
        return Ok(());
    }

    let legacy_cache: LegacyCache = json::load_json_file(&legacy_cache_file_path)?;
    let mut cache = Cache::create(cache_file_path)?;

    cache.reset(legacy_cache.paths);

    Ok(())
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
            info!("[DRY RUN] {}", str.as_ref());
        } else {
            info!("{}", str.as_ref());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;
    use temp_dir_builder::TempDirectoryBuilder;

    use crate::{cache::Cache, config::Config, import_legacy_cache_file, import_legacy_config_file};

    #[test]
    fn test_import_legacy_config_file_dont_exist() {
        import_legacy_config_file("don't exist", "don't exist").unwrap();
    }

    #[test]
    fn test_import_legacy_config_file() {
        let temp_dir = TempDirectoryBuilder::default().build().unwrap();
        let legacy_cache_content = json!({"searchPaths": [
            temp_dir.path()
          ],
          "ignoredPaths": [
            "a"
          ],
          "whitelist": [
            "*.hey"
          ]
        })
        .to_string();
        let dir_path = temp_dir.path().to_path_buf();
        let legacy_config_file_path = dir_path.join("legacy.json");
        let config_file_path = dir_path.join("config.json");
        std::fs::write(&legacy_config_file_path, legacy_cache_content).unwrap();
        import_legacy_config_file(&legacy_config_file_path, &config_file_path).unwrap();
        let config = Config::load_from_file(&config_file_path).unwrap();
        assert!(config.search_directories.contains(&dir_path));
        assert!(config.ignored_directories.contains(&PathBuf::from("a")));
        assert!(config.whitelist_patterns.contains("*.hey"));
    }

    #[test]
    fn test_import_legacy_cache_file_dont_exist() {
        import_legacy_cache_file("dont exist", "dont exist").unwrap();
    }

    #[test]
    fn test_import_legacy_cache_file() {
        let legacy_content = r#"{"paths":["a", "b"]}"#;
        let temp_dir = TempDirectoryBuilder::default()
            .add_text_file("legacy.json", legacy_content)
            .build()
            .unwrap();
        let cache_file_path = temp_dir.path().join("cache.db");
        import_legacy_cache_file(temp_dir.path().join("legacy.json"), &cache_file_path).unwrap();
        let cache = Cache::load_from_file(&cache_file_path).unwrap();
        let paths = cache.paths();
        assert_eq!(2, paths.len());
        assert!(paths.contains(&PathBuf::from("a")));
        assert!(paths.contains(&PathBuf::from("b")));
    }
}
