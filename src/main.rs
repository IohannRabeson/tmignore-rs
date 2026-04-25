#![warn(clippy::pedantic)]

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
use std::{backtrace::BacktraceStatus, path::Path};

use crate::{
    cache::Cache,
    commands::{path::Paths, stats::Stats},
    config::Config,
    legacy_cache::LegacyCache,
};

const CONFIG_FILE_PATH: &str = "~/.config/tmignore-rs/config.json";
const CACHE_FILE_PATH: &str = "~/Library/Caches/tmignore-rs/cache.db";
const LEGACY_CONFIG_FILE_PATH: &str = "~/.config/tmignore/config.json";
const LEGACY_CACHE_FILE_PATH: &str = "~/Library/Caches/tmignore/cache.json";

#[derive(Parser)]
#[command(about, long_about = None)]
#[command(version = option_env!("TMIGNORE_RS_VERSION").unwrap_or("<Local Build>"))]
#[command(disable_help_flag = true)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
    /// Enable verbose logging
    #[arg(short, long)]
    verbose: bool,
    /// Specify the configuration file path
    #[arg(long, default_value = CONFIG_FILE_PATH, hide = true)]
    config: String,
    /// Specify the cache file path
    #[arg(long, default_value = CACHE_FILE_PATH, hide = true)]
    cache: String,
    /// Specify the legacy configuration file path
    #[arg(long, default_value = LEGACY_CONFIG_FILE_PATH, hide = true)]
    legacy_config: String,
    /// Specify the legacy cache file path
    #[arg(long, default_value = LEGACY_CACHE_FILE_PATH, hide = true)]
    legacy_cache: String,
    #[arg(long, action = clap::ArgAction::Help, global = true)]
    help: Option<bool>,
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
    /// Print the paths of the files used by the application
    Path {
        #[command(subcommand)]
        path: Paths,
    },
    /// Print statistics
    Stats {
        #[command(subcommand)]
        stat: Stats,
    },
}

fn program(cli: Cli, redirect_log_to_console: bool) -> anyhow::Result<()> {
    let mut cli = cli;
    expand_paths(&mut cli);
    setup_log(cli.verbose, redirect_log_to_console)?;
    import_legacy_config_file(&cli.legacy_config, &cli.config)?;
    import_legacy_cache_file(&cli.legacy_cache, &cli.cache)?;
    match cli.command {
        Commands::Run { dry_run, details } => {
            let mut cache = Cache::open_or_create(&cli.cache)?;
            let config = Config::load_or_create_file(&cli.config)?;

            commands::run::execute(&config, &mut cache, dry_run, details)
        }
        Commands::List { zero_separator } => {
            let cache = Cache::open_or_create(&cli.cache)?;
            let separator = if zero_separator { '\0' } else { '\n' };

            commands::list::execute(&cache, &mut std::io::stdout(), separator)
        }
        Commands::Reset { dry_run, details } => {
            let mut cache = Cache::open_or_create(&cli.cache)?;

            commands::reset::execute(&mut cache, dry_run, details);

            Ok(())
        }
        Commands::Monitor { dry_run, details } => {
            let mut cache = Cache::open_or_create(&cli.cache)?;
            let global_gitignore = git::get_global_git_ignore();

            commands::monitor::execute(
                &cli.config,
                global_gitignore.as_ref(),
                &mut cache,
                dry_run,
                details,
            )
        }
        Commands::Path { path } => commands::path::execute(&cli, path, &mut std::io::stdout()),
        Commands::Stats { stat } => {
            let cache = Cache::open(&cli.cache)?;

            commands::stats::execute(&cache, &mut std::io::stdout(), stat)
        }
    }?;

    Ok(())
}

fn expand_paths(cli: &mut Cli) {
    cli.config = shellexpand::tilde(&cli.config).to_string();
    cli.cache = shellexpand::tilde(&cli.cache).to_string();
    cli.legacy_config = shellexpand::tilde(&cli.legacy_config).to_string();
    cli.legacy_cache = shellexpand::tilde(&cli.legacy_cache).to_string();
}

static INIT_LOG: std::sync::Once = std::sync::Once::new();

fn setup_log(verbose: bool, redirect_log_to_console: bool) -> anyhow::Result<()> {
    let mut result = Ok(());
    INIT_LOG.call_once(|| {
        result = setup_log_impl(verbose, redirect_log_to_console);
    });
    result
}

fn setup_log_impl(verbose: bool, console_enabled: bool) -> anyhow::Result<()> {
    let level = if verbose {
        log::LevelFilter::Debug
    } else {
        log::LevelFilter::Info
    };
    let os_logger: Box<dyn log::Log> = Box::new(oslog::OsLogger::new("com.irabeson.tmignore-rs"));

    let mut dispatch = fern::Dispatch::new()
        .level(level)
        .filter(|metadata| metadata.target().starts_with("tmignore_rs"))
        .chain(std::io::stdout());

    if console_enabled {
        dispatch = dispatch.chain(os_logger);
    }

    dispatch.apply()?;

    if verbose {
        info!("Verbose mode enabled");
    }

    Ok(())
}

fn main() {
    if let Err(error) = program(Cli::parse(), true) {
        error!("Error: {error}");
        if error.backtrace().status() == BacktraceStatus::Captured {
            error!("{}", error.backtrace());
        }
    }
}

fn import_legacy_config_file(
    legacy_config_file_path: impl AsRef<Path>,
    config_file_path: impl AsRef<Path>,
) -> anyhow::Result<()> {
    let legacy_config_file_path = legacy_config_file_path.as_ref();
    let config_file_path = config_file_path.as_ref();
    if !legacy_config_file_path.is_file() || config_file_path.is_file() {
        return Ok(());
    }
    info!(
        "Importing legacy config '{}'...",
        legacy_config_file_path.display()
    );
    let legacy_config = json::load_json_file(legacy_config_file_path)?;
    let new_config = Config::from(&legacy_config);
    if let Some(parent) = config_file_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    json::save_json_file(config_file_path, &new_config)?;
    info!("Create new config file '{}'", config_file_path.display());
    info!("You can delete '{}' now", legacy_config_file_path.display());
    Ok(())
}

fn import_legacy_cache_file(
    legacy_cache_file_path: impl AsRef<Path>,
    cache_file_path: impl AsRef<Path>,
) -> Result<(), anyhow::Error> {
    let legacy_cache_file_path = legacy_cache_file_path.as_ref();
    let cache_file_path = cache_file_path.as_ref();

    if !legacy_cache_file_path.is_file() || cache_file_path.is_file() {
        return Ok(());
    }

    let legacy_cache: LegacyCache = json::load_json_file(legacy_cache_file_path)?;
    let mut cache = Cache::create(cache_file_path)?;

    cache.reset(legacy_cache.paths);

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        path::{Path, PathBuf},
        time::Duration,
    };

    use serde_json::json;
    use serial_test::serial;
    use temp_dir_builder::{TempDirectory, TempDirectoryBuilder};

    use crate::{
        Cli, cache::Cache, config::Config, import_legacy_cache_file, import_legacy_config_file,
        json::save_json_file, program,
    };

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
        let cache = Cache::open(&cache_file_path).unwrap();
        let paths = cache.paths();
        assert_eq!(2, paths.len());
        assert!(paths.contains(&PathBuf::from("a")));
        assert!(paths.contains(&PathBuf::from("b")));
    }

    fn create_repository(root_directory: impl AsRef<Path>) -> TempDirectory {
        let root_directory = root_directory.as_ref();
        if root_directory.exists() && root_directory.is_dir() {
            std::fs::remove_dir_all(&root_directory).unwrap();
        }
        let repository_path = root_directory.join("repository");
        let mut config = Config::default();
        config.search_directories.clear();
        config.search_directories.insert(repository_path.clone());
        let temp_dir = TempDirectoryBuilder::default()
            .root_folder(root_directory)
            .add_text_file("config.json", serde_json::to_string(&config).unwrap())
            .add_text_file("repository/.gitignore", "a\nb\n")
            .add_empty_file("repository/a")
            .add_empty_file("repository/b")
            .add_empty_file("repository/c")
            .build()
            .unwrap();

        crate::commands::tests::init_git_repository(repository_path);

        temp_dir
    }

    #[test]
    fn test_program_run() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let temp_dir_path = root.join("test_program_run");
        let _temp_dir = create_repository(&temp_dir_path);
        let config_file_path = temp_dir_path.join("config.json");
        let cache_file_path = temp_dir_path.join("cache.db");
        let cli = Cli {
            command: crate::Commands::Run {
                dry_run: false,
                details: false,
            },
            verbose: false,
            config: config_file_path.to_string_lossy().to_string(),
            cache: cache_file_path.to_string_lossy().to_string(),
            legacy_config: String::new(),
            legacy_cache: String::new(),
            help: None,
        };
        let a_file_path = temp_dir_path.join("repository").join("a");
        let b_file_path = temp_dir_path.join("repository").join("b");
        let c_file_path = temp_dir_path.join("repository").join("c");

        assert!(!crate::timemachine::tests::is_excluded_from_time_machine(
            &a_file_path
        ));
        assert!(!crate::timemachine::tests::is_excluded_from_time_machine(
            &b_file_path
        ));
        assert!(!crate::timemachine::tests::is_excluded_from_time_machine(
            &c_file_path
        ));
        if let Err(error) = program(cli, false) {
            panic!("program returned an error: {}", error);
        }
        assert!(crate::timemachine::tests::is_excluded_from_time_machine(
            &a_file_path
        ));
        assert!(crate::timemachine::tests::is_excluded_from_time_machine(
            &b_file_path
        ));
        assert!(!crate::timemachine::tests::is_excluded_from_time_machine(
            &c_file_path
        ));
    }

    #[test]
    #[serial]
    fn test_program_monitor() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let temp_dir_path = root.join("test_program_monitor");
        let _temp_dir = {
            let root_directory = &temp_dir_path;
            if root_directory.exists() && root_directory.is_dir() {
                std::fs::remove_dir_all(&root_directory).unwrap();
            }
            let repository_path = root_directory.join("repository");
            let mut config = Config::default();
            config.debounce_duration = Duration::from_secs(1);
            config.search_directories.clear();
            config.search_directories.insert(repository_path.clone());
            let temp_dir = TempDirectoryBuilder::default()
                .root_folder(root_directory)
                .add_text_file("config.json", serde_json::to_string(&config).unwrap())
                .add_text_file("repository/.gitignore", "a\nb\n")
                .build()
                .unwrap();

            crate::commands::tests::init_git_repository(repository_path);

            temp_dir
        };
        let config_file_path = temp_dir_path.join("config.json");
        let cache_file_path = temp_dir_path.join("cache.db");
        let cli = Cli {
            command: crate::Commands::Monitor {
                dry_run: false,
                details: false,
            },
            verbose: true,
            config: config_file_path.to_string_lossy().to_string(),
            cache: cache_file_path.to_string_lossy().to_string(),
            legacy_config: String::new(),
            legacy_cache: String::new(),
            help: None,
        };
        let a_file_path = temp_dir_path.join("repository").join("a");
        let b_file_path = temp_dir_path.join("repository").join("b");
        let c_file_path = temp_dir_path.join("repository").join("c");

        assert!(!crate::timemachine::tests::is_excluded_from_time_machine(
            &a_file_path
        ));
        assert!(!crate::timemachine::tests::is_excluded_from_time_machine(
            &b_file_path
        ));
        assert!(!crate::timemachine::tests::is_excluded_from_time_machine(
            &c_file_path
        ));
        let handle = std::thread::spawn(move || program(cli, false).unwrap());
        std::thread::sleep(Duration::from_millis(500));
        std::fs::write(&a_file_path, "").unwrap();
        std::fs::write(&b_file_path, "").unwrap();
        std::fs::write(&c_file_path, "").unwrap();
        std::thread::sleep(Duration::from_secs(5));
        crate::commands::tests::send_sigint();
        handle.join().unwrap();

        assert!(crate::timemachine::tests::is_excluded_from_time_machine(
            &a_file_path
        ));
        assert!(crate::timemachine::tests::is_excluded_from_time_machine(
            &b_file_path
        ));
        assert!(!crate::timemachine::tests::is_excluded_from_time_machine(
            &c_file_path
        ));
    }

    #[test]
    #[serial]
    fn test_program_monitor_reload_config() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let temp_dir_path = root.join("test_program_monitor_reload_config");
        let temp_dir = {
            let root_directory = &temp_dir_path;
            if root_directory.exists() && root_directory.is_dir() {
                std::fs::remove_dir_all(&root_directory).unwrap();
            }
            let repository_path = root_directory.join("repository");
            let mut config = Config::default();
            config.debounce_duration = Duration::from_secs(1);
            config.search_directories.clear();
            config
                .search_directories
                .insert(root_directory.join("empty_dir"));
            let temp_dir = TempDirectoryBuilder::default()
                .root_folder(root_directory)
                .add_directory("empty_dir")
                .add_text_file("config.json", serde_json::to_string(&config).unwrap())
                .add_text_file("repository/.gitignore", "a\nb\n")
                .build()
                .unwrap();

            crate::commands::tests::init_git_repository(repository_path);

            temp_dir
        };
        let config_file_path = temp_dir_path.join("config.json");
        let cache_file_path = temp_dir_path.join("cache.db");
        let cli = Cli {
            command: crate::Commands::Monitor {
                dry_run: false,
                details: false,
            },
            verbose: true,
            config: config_file_path.to_string_lossy().to_string(),
            cache: cache_file_path.to_string_lossy().to_string(),
            legacy_config: String::new(),
            legacy_cache: String::new(),
            help: None,
        };
        let a_file_path = temp_dir_path.join("repository").join("a");
        let b_file_path = temp_dir_path.join("repository").join("b");
        let c_file_path = temp_dir_path.join("repository").join("c");
        let config_file_path = temp_dir_path.join("config.json");
        assert!(!crate::timemachine::tests::is_excluded_from_time_machine(
            &a_file_path
        ));
        assert!(!crate::timemachine::tests::is_excluded_from_time_machine(
            &b_file_path
        ));
        assert!(!crate::timemachine::tests::is_excluded_from_time_machine(
            &c_file_path
        ));
        let handle = std::thread::spawn(move || program(cli, false).unwrap());
        std::thread::sleep(Duration::from_secs(5));
        std::fs::write(&a_file_path, "").unwrap();
        std::fs::write(&b_file_path, "").unwrap();
        std::fs::write(&c_file_path, "").unwrap();
        let mut config = Config::default();
        config.debounce_duration = Duration::from_secs(1);
        config.search_directories.clear();
        config
            .search_directories
            .insert(temp_dir.path().join("repository"));
        save_json_file(config_file_path, &config).unwrap();
        std::thread::sleep(Duration::from_secs(5));
        crate::commands::tests::send_sigint();
        handle.join().unwrap();

        assert!(crate::timemachine::tests::is_excluded_from_time_machine(
            &a_file_path
        ));
        assert!(crate::timemachine::tests::is_excluded_from_time_machine(
            &b_file_path
        ));
        assert!(!crate::timemachine::tests::is_excluded_from_time_machine(
            &c_file_path
        ));
    }

    /// Test the monitor is resilient to invalid configuration.
    /// The test is a success if the program closes gracefully.
    #[test]
    #[serial]
    fn test_program_monitor_reload_config_error() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let temp_dir_path = root.join("test_program_monitor_reload_config_error");
        let _temp_dir = {
            let root_directory = &temp_dir_path;
            if root_directory.exists() && root_directory.is_dir() {
                std::fs::remove_dir_all(&root_directory).unwrap();
            }
            let repository_path = root_directory.join("repository");
            let mut config = Config::default();
            config.debounce_duration = Duration::from_secs(1);
            config.search_directories.clear();
            config
                .search_directories
                .insert(root_directory.join("empty_dir"));
            let temp_dir = TempDirectoryBuilder::default()
                .root_folder(root_directory)
                .add_directory("empty_dir")
                .add_text_file("config.json", serde_json::to_string(&config).unwrap())
                .add_text_file("repository/.gitignore", "a\nb\n")
                .build()
                .unwrap();

            crate::commands::tests::init_git_repository(repository_path);

            temp_dir
        };
        let config_file_path = temp_dir_path.join("config.json");
        let cache_file_path = temp_dir_path.join("cache.db");
        let cli = Cli {
            command: crate::Commands::Monitor {
                dry_run: false,
                details: false,
            },
            verbose: true,
            config: config_file_path.to_string_lossy().to_string(),
            cache: cache_file_path.to_string_lossy().to_string(),
            legacy_config: String::new(),
            legacy_cache: String::new(),
            help: None,
        };
        let config_file_path = temp_dir_path.join("config.json");
        let handle = std::thread::spawn(move || program(cli, false).unwrap());
        std::thread::sleep(Duration::from_secs(5));
        // Write an invalid config
        std::fs::write(config_file_path, "").unwrap();
        std::thread::sleep(Duration::from_secs(5));
        crate::commands::tests::send_sigint();
        handle.join().unwrap();
    }
}
