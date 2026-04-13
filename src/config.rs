use std::{
    collections::BTreeSet,
    fs::File,
    io::{BufReader, Read},
    path::{Path, PathBuf},
    time::Duration,
};

use log::info;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::legacy_config::LegacyConfig;

#[derive(Serialize, Deserialize, Debug)]
pub struct Config {
    /// The list of the directories to scan.
    pub search_directories: BTreeSet<PathBuf>,
    /// The list of directories to ignore.
    pub ignored_directories: BTreeSet<PathBuf>,
    /// The list of patterns filtering the entries that should always be included in backup.
    pub whitelist_patterns: BTreeSet<String>,
    /// Count of threads used for scanning the file system.
    pub threads: usize,
    /// Debounce duration.
    #[serde(
        serialize_with = "serialize_human_time",
        deserialize_with = "deserialize_human_time"
    )]
    pub debounce_duration: Duration,
}

fn serialize_human_time<S>(value: &Duration, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let s = humantime::format_duration(*value);

    serializer.serialize_str(&s.to_string())
}

fn deserialize_human_time<'de, D>(deserializer: D) -> Result<Duration, D::Error>
where
    D: Deserializer<'de>,
{
    let s: String = Deserialize::deserialize(deserializer)?;

    match humantime::parse_duration(&s) {
        Ok(duration) => Ok(duration),
        Err(error) => {
            const HELP_URL: &str =
                "https://github.com/IohannRabeson/tmignore-rs?tab=readme-ov-file#debounce_duration";

            Err(serde::de::Error::custom(format!(
                "Invalid duration: {error}. See {HELP_URL} for help."
            )))
        }
    }
}

impl From<&LegacyConfig> for Config {
    fn from(legacy_config: &LegacyConfig) -> Self {
        Self {
            search_directories: legacy_config.search_directories.clone(),
            ignored_directories: legacy_config.ignored_directories.clone(),
            whitelist_patterns: legacy_config.whitelist_patterns.clone(),
            ..Default::default()
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub struct ValidationError {
    pub fails: Vec<ValidationFail>,
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Validation error:")?;
        for fail in &self.fails {
            writeln!(f, " - {fail}")?;
        }
        Ok(())
    }
}

#[derive(thiserror::Error, Debug, PartialEq)]
pub enum ValidationFail {
    #[error("File in search_directories: '{0}'")]
    FileInSearchPaths(PathBuf),
    #[error("File in ignored_directories: '{0}'")]
    FileInIgnoredDirectories(PathBuf),
    #[error("Path not found '{0}'")]
    NotFound(PathBuf),
    #[error("No search directories")]
    NoSearchDirectories,
}

impl Config {
    pub const DEFAULT_DEBOUNCE_DURATION_SECS: u64 = 2;
    pub const DEFAULT_THREADS: usize = 4;

    pub fn load_or_create_file(file_path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let file_path = file_path.as_ref();

        if file_path.is_file() {
            Self::load_from_file(file_path)
        } else {
            let mut default_config = Self::default();

            let parent_directory = file_path.parent().ok_or(anyhow::anyhow!(
                "Can't get a parent path for '{}'",
                file_path.display()
            ))?;

            std::fs::create_dir_all(parent_directory)?;

            let file = File::create_new(file_path)?;

            serde_json::to_writer_pretty(file, &default_config)?;

            info!("Created configuration file '{}'", file_path.display());

            Self::expand(&mut default_config);

            Ok(default_config)
        }
    }

    pub fn load_from_file(file_path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let file_path = file_path.as_ref();
        info!("Load configuration '{}'", file_path.display());
        Self::load(File::open(file_path)?)
    }

    pub fn load(reader: impl Read) -> anyhow::Result<Self> {
        let reader = BufReader::new(reader);
        let mut config = serde_json::from_reader(reader)?;
        Self::expand(&mut config);
        Self::validate(&config)?;
        Ok(config)
    }

    fn expand(config: &mut Config) {
        config.search_directories = Self::expand_paths(&config.search_directories);
        config.ignored_directories = Self::expand_paths(&config.ignored_directories);
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

    fn validate(config: &Config) -> Result<(), ValidationError> {
        let mut fails = Vec::new();

        for path in &config.search_directories {
            if path.is_file() {
                fails.push(ValidationFail::FileInSearchPaths(path.clone()));
            } else if !path.is_dir() {
                fails.push(ValidationFail::NotFound(path.clone()));
            }
        }

        for path in &config.ignored_directories {
            if path.is_file() {
                fails.push(ValidationFail::FileInIgnoredDirectories(path.clone()));
            }
        }

        if config.search_directories.is_empty() {
            fails.push(ValidationFail::NoSearchDirectories);
        }

        if fails.is_empty() {
            Ok(())
        } else {
            Err(ValidationError { fails })
        }
    }

    pub fn reload_file(&mut self, file_path: impl AsRef<Path>) -> anyhow::Result<()> {
        self.reload(std::fs::File::open(file_path)?)?;

        Ok(())
    }

    pub fn reload(&mut self, reader: impl Read) -> anyhow::Result<()> {
        *self = Self::load(reader)?;
        Self::expand(self);
        Self::validate(self)?;
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
            threads: Self::DEFAULT_THREADS,
            debounce_duration: Duration::from_secs(Self::DEFAULT_DEBOUNCE_DURATION_SECS),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use temp_dir_builder::TempDirectoryBuilder;

    use crate::config::{Config, ValidationError};

    #[test]
    fn test_expand_default() {
        let temp_dir = TempDirectoryBuilder::default().build().unwrap();
        let config_file_path = temp_dir.path().join("does_not_exist.json");
        let config = Config::load_or_create_file(&config_file_path).unwrap();

        assert!(config.search_directories.len() > 0);
        assert!(config.ignored_directories.len() > 0);
        assert!(
            config
                .search_directories
                .iter()
                .all(|path| !path.to_str().unwrap().contains('~'))
        );
        assert!(
            config
                .ignored_directories
                .iter()
                .all(|path| !path.to_str().unwrap().contains('~'))
        );
        assert!(config_file_path.is_file());
    }

    #[test]
    fn test_expand_loaded() {
        let json = r#"
{ "search_directories": ["~"],
  "ignored_directories": [],
  "whitelist_patterns": [],
  "threads": 4,
  "debounce_duration": "5s" }
"#;
        let config = Config::load(json.as_bytes()).unwrap();
        let search_directories: Vec<_> = config.search_directories.iter().collect();

        assert_eq!(1, search_directories.len());
        assert!(!search_directories[0].to_str().unwrap().contains('~'));
    }

    #[test]
    fn test_expand_reloaded() {
        let json = r#"
{ "search_directories": ["~"],
  "ignored_directories": [],
  "whitelist_patterns": [],
  "threads": 4,
  "debounce_duration": "5s" }
"#;
        let mut config = Config::load(json.as_bytes()).unwrap();
        config.reload(json.as_bytes()).unwrap();

        let search_directories: Vec<_> = config.search_directories.iter().collect();
        assert_eq!(1, search_directories.len());
        assert!(!search_directories[0].to_str().unwrap().contains('~'));
    }

    #[test]
    fn test_missing_required_field() {
        let json = r#"
{
"ignored_directories": [],
"whitelist_patterns": [] }
"#;
        let result = Config::load(json.as_bytes());
        assert!(result.is_err());
        let _error = result
            .unwrap_err()
            .downcast_ref::<serde_json::Error>()
            .expect("downcast failed");
    }

    #[test]
    fn test_search_paths_does_not_exist() {
        let json = r#"
{
"search_directories": ["/does_not_exist"],
"ignored_directories": [],
"whitelist_patterns": [],
"threads": 4,
"debounce_duration": "5s" }
"#;
        let result = Config::load(json.as_bytes());
        let error = result.unwrap_err();
        let error = error
            .downcast_ref::<ValidationError>()
            .expect("downcast failed");
        assert!(error.fails.len() > 0);
        let expected = crate::config::ValidationFail::NotFound(PathBuf::from("/does_not_exist"));
        assert!(error.fails.contains(&expected));
    }

    #[test]
    fn test_no_search_paths() {
        let json = r#"
{
"search_directories": [],
"ignored_directories": [],
"whitelist_patterns": [],
"threads": 4,
"debounce_duration": "5s" }
"#;
        let result = Config::load(json.as_bytes());
        let error = result.unwrap_err();
        let error = error
            .downcast_ref::<ValidationError>()
            .expect("downcast failed");
        assert!(error.fails.len() > 0);
        assert!(
            error
                .fails
                .contains(&crate::config::ValidationFail::NoSearchDirectories)
        );
    }

    #[test]
    fn test_reload() {
        let json = r#"
{ "search_directories": ["~"],
  "ignored_directories": [],
  "whitelist_patterns": [],
  "threads": 4,
  "debounce_duration": "5s" }
"#;
        let mut config = Config::load(json.as_bytes()).unwrap();
        let json = r#"
{ "search_directories": ["~"],
  "ignored_directories": ["~"],
  "whitelist_patterns": [],
  "threads": 4,
  "debounce_duration": "5s" }
"#;
        config.reload(json.as_bytes()).unwrap();

        assert!(!config.ignored_directories.is_empty());
    }

    #[test]
    fn test_reload_error() {
        let json = r#"
{ "search_directories": ["~"],
  "ignored_directories": [],
  "whitelist_patterns": [],
  "threads": 4,
  "debounce_duration": "5s" }
"#;
        let mut config = Config::load(json.as_bytes()).unwrap();
        let invalid_json = r#"
{ "search_directories": [],
  "ignored_directories": [],
  "whitelist_patterns": [],
  "threads": 4,
  "debounce_duration": "5s" }
"#;
        assert!(config.reload(invalid_json.as_bytes()).is_err());
        assert_eq!(1, config.search_directories.len());
    }
}
