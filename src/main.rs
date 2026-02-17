mod cache;
mod config;
mod git;

use std::error::Error;

use clap::{Parser, Subcommand};

use crate::{
    cache::{Cache, LegacyCache, OpenOrCreate, OpenOrCreateError},
    config::Config,
};

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Run,
    List,
    Reset,
}

fn main() -> Result<(), Box<dyn Error>> {
    const CONFIG_FILE_PATH: &str = "~/.config/tmignore/config.json";

    let cli = Cli::parse();
    let config = Config::load_or_create_file(shellexpand::tilde(CONFIG_FILE_PATH).to_string())?;
    let mut cache = open_cache()?;

    match cli.command {
        Commands::Run => run_command::execute(&config, &mut cache),
        Commands::List => list_command::execute(&config),
        Commands::Reset => reset_command::execute(&config),
    }?;

    cache.save_to_file()?;

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

mod run_command {
    use std::{collections::BTreeSet, error::Error};

    use crate::{cache::Cache, config::Config, git};

    pub fn execute(config: &Config, cache: &mut Cache) -> Result<(), Box<dyn Error>> {
        let mut repositories = BTreeSet::new();
        let mut exclusions = BTreeSet::new();

        if let Some((rx, thread_handle)) =
            git::find_repositories(&config.search_directories, &config.ignored_directories)
        {
            while let Ok(repository_path) = rx.recv() {
                repositories.insert(repository_path.clone());

                let ignored_files = git::find_ignored_files(&repository_path);

                for ignored_file in ignored_files {
                    exclusions.insert(ignored_file);
                }
            }
            thread_handle.join().unwrap();

            println!("Found {} repositories", repositories.len());

            let diff = cache.find_diff(&exclusions);

            for path in &diff.added {
                println!("+ {}", path.display());
            }
            for path in &diff.removed {
                println!("- {}", path.display());
            }

            cache.write(exclusions);
        }

        Ok(())
    }
}

mod list_command {
    use std::error::Error;

    use crate::config::Config;

    pub fn execute(config: &Config) -> Result<(), Box<dyn Error>> {
        println!("list");
        Ok(())
    }
}

mod reset_command {
    use crate::config::Config;

    use std::error::Error;

    pub fn execute(config: &Config) -> Result<(), Box<dyn Error>> {
        println!("reset");
        Ok(())
    }
}
