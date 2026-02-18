use std::{
    collections::BTreeSet,
    fs::File,
    io::BufReader,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct Config {
    #[serde(rename = "searchPaths")]
    pub search_directories: BTreeSet<PathBuf>,
    #[serde(rename = "ignoredPaths")]
    pub ignored_directories: BTreeSet<PathBuf>,
    #[serde(rename = "whitelist")]
    pub whitelist_patterns: BTreeSet<String>,
}

#[derive(thiserror::Error, Debug)]
pub enum LoadFromFileError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Validation(#[from] ValidationError),
}

#[derive(thiserror::Error, Debug)]
pub enum ValidationError {
    #[error("File in searchPaths: {0}")]
    FileInSearchPaths(PathBuf),
    #[error("File in ignoredPaths: {0}")]
    FileInIgnoredPaths(PathBuf),
}

fn expand_paths(paths: &BTreeSet<PathBuf>) -> BTreeSet<PathBuf> {
    let mut results = BTreeSet::new();

    for path in paths {
        if let Some(path_str) = path.to_str()
            && let Ok(expanded) = shellexpand::full(path_str)
        {
            results.insert(PathBuf::from(expanded.to_string()));
        }
    }

    results
}

impl Config {
    pub fn load_or_create_file(file_path: impl AsRef<Path>) -> Result<Self, LoadFromFileError> {
        let file_path = file_path.as_ref();

        let mut config = if file_path.is_file() {
            Self::load_from_file(file_path)
        } else {
            let default_config = Self::default();

            std::fs::create_dir_all(file_path.parent().unwrap())?;

            let file = File::create_new(file_path)?;

            serde_json::to_writer_pretty(file, &default_config)?;

            println!("Created configuration file '{}'", file_path.display());

            Ok(default_config)
        };

        if let Ok(config) = config.as_mut() {
            config.search_directories = expand_paths(&config.search_directories);
            config.ignored_directories = expand_paths(&config.ignored_directories);

            Self::validate(config)?;
        }

        config
    }

    fn load_from_file(file_path: impl AsRef<Path>) -> Result<Self, LoadFromFileError> {
        let file = File::open(file_path)?;
        let reader = BufReader::new(file);

        Ok(serde_json::from_reader(reader)?)
    }

    fn validate(config: &Config) -> Result<(), ValidationError> {
        for path in &config.search_directories {
            if path.is_file() {
                return Err(ValidationError::FileInSearchPaths(path.clone()));
            }
        }

        for path in &config.ignored_directories {
            if path.is_file() {
                return Err(ValidationError::FileInIgnoredPaths(path.clone()));
            }
        }

        Ok(())
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            search_directories: BTreeSet::from(["~".into()]),
            ignored_directories: BTreeSet::from([
                "~/.Trash".into(),
                "~/Applications".into(),
                "~/Downloads".into(),
                "~/Library".into(),
                "~/Music/iTunes".into(),
                "~/Music/Music".into(),
                "~/Pictures/Photos Library.photoslibrary".into(),
            ]),
            whitelist_patterns: BTreeSet::new(),
        }
    }
}

#[cfg(test)]
mod tests {}
