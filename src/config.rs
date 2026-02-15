use std::{
    fs::File,
    io::BufReader,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct Config {
    #[serde(rename = "searchPaths")]
    pub search_directories: Vec<PathBuf>,
    #[serde(rename = "ignoredPaths")]
    pub ignored_directories: Vec<PathBuf>,
    pub whitelist: Vec<PathBuf>,
}

#[derive(thiserror::Error, Debug)]
pub enum LoadFromFileError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

fn expand_paths(paths: &mut [PathBuf]) {
    for path in paths {
        if let Some(path_str) = path.to_str() 
        && let Ok(expanded) = shellexpand::full(path_str) {
            *path = PathBuf::from(expanded.to_string())
        }
    }
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

            Ok(default_config)
        };

        if let Ok(config) = config.as_mut() {
            expand_paths(&mut config.search_directories);
            expand_paths(&mut config.ignored_directories);
            expand_paths(&mut config.whitelist);
        }

        config
    }

    fn load_from_file(file_path: impl AsRef<Path>) -> Result<Self, LoadFromFileError> {
        let file = File::open(file_path)?;
        let reader = BufReader::new(file);

        Ok(serde_json::from_reader(reader)?)
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            search_directories: vec!["~".into()],
            ignored_directories: vec![
                "~/.Trash".into(),
                "~/Applications".into(),
                "~/Downloads".into(),
                "~/Library".into(),
                "~/Music/iTunes".into(),
                "~/Music/Music".into(),
                "~/Pictures/Photos Library.photoslibrary".into(),
            ],
            whitelist: vec![],
        }
    }
}
