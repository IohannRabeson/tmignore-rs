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
    Run {
        #[arg(short, long)]
        dry_run: bool,
    },
    List,
    Reset,
}

fn main() -> Result<(), Box<dyn Error>> {
    const CONFIG_FILE_PATH: &str = "~/.config/tmignore/config.json";

    let cli = Cli::parse();
    let config_file_path = shellexpand::tilde(CONFIG_FILE_PATH).to_string();
    let config = Config::load_or_create_file(config_file_path)?;
    let mut cache = open_cache()?;
    let whitelist = RegexSet::new(config.whitelist_patterns.iter().filter_map(|pattern| {
        match fnmatch_regex::glob_to_regex_pattern(pattern) {
            Ok(pattern) => Some(pattern),
            Err(error) => {
                eprintln!("Invalid whitelist pattern '{}': {}", pattern, error);
                None
            }
        }
    }))?;

    match cli.command {
        Commands::Run { dry_run } => run_command::execute(&config, &mut cache, dry_run, &whitelist),
        Commands::List => list_command::execute(&cache),
        Commands::Reset => reset_command::execute(&mut cache),
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

fn apply_diff_and_print(diff: &crate::diff::Diff, dry_run: bool) -> Vec<&Path> {
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

    for path in &diff.added {
        if !add_failed_paths.contains(path) {
            println!("+ {}", path.display());
        }
    }

    for path in &diff.removed {
        println!("- {}", path.display());
    }

    for error in &errors {
        eprintln!("Error: {}: {}", error.path.display(), error.error)
    }

    errors
        .into_iter()
        .filter(|error| error.added)
        .map(|entry| entry.path)
        .collect()
}

mod run_command {
    use std::{collections::BTreeSet, error::Error};

    use regex::RegexSet;

    use crate::{cache::Cache, config::Config, git};

    pub fn execute(
        config: &Config,
        cache: &mut Cache,
        dry_run: bool,
        whitelist: &RegexSet,
    ) -> Result<(), Box<dyn Error>> {
        let mut repositories = BTreeSet::new();
        let mut exclusions = BTreeSet::new();

        if let Some((rx, thread_handle)) =
            git::find_repositories(&config.search_directories, &config.ignored_directories)
        {
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

            println!("Found {} repositories", repositories.len());

            if dry_run {
                println!("Dry run mode enabled");
            }

            let diff = cache.find_diff(&exclusions);

            let paths_failed_to_add = super::apply_diff_and_print(&diff, dry_run);

            for path in paths_failed_to_add {
                exclusions.remove(path);
            }

            if !dry_run {
                cache.write(exclusions);
            }
        }

        Ok(())
    }
}

mod list_command {
    use std::error::Error;

    use crate::cache::Cache;

    pub fn execute(cache: &Cache) -> Result<(), Box<dyn Error>> {
        for path in cache.paths() {
            println!("{}", path.display());
        }
        Ok(())
    }
}

mod reset_command {
    use crate::cache::Cache;

    use std::{collections::BTreeSet, error::Error};

    pub fn execute(cache: &mut Cache) -> Result<(), Box<dyn Error>> {
        let diff = cache.find_diff(&BTreeSet::new());

        super::apply_diff_and_print(&diff, false);

        cache.write([]);

        Ok(())
    }
}
