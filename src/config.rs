use std::{
    collections::BTreeSet,
    fs::File,
    io::{BufReader, Read},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::legacy_config::LegacyConfig;

#[derive(Serialize, Deserialize)]
pub struct Config {
    /// The list of the directories to scan.
    pub search_directories: BTreeSet<PathBuf>,
    /// The list of directories to ignore.
    pub ignored_directories: BTreeSet<PathBuf>,
    /// The list of patterns filtering the entries that should always be included in backup.
    pub whitelist_patterns: BTreeSet<String>,
    /// Count of threads used for scanning the file system.
    pub threads: usize,
    /// Monitoring interval in seconds.
    pub monitor_interval_secs: u64,
}

pub mod modifier {
    use std::{
        io::{BufReader, Read, Seek},
        path::{Path, PathBuf},
    };

    use crate::config::{Config, LoadError, Modifier};

    /// Create a modifier loading a PList file.
    /// If the file does not exist or is not a file, then
    /// a modifier doing nothing is returned.
    pub fn time_machine_plist(file_path: impl AsRef<Path>) -> Result<impl Modifier, LoadError> {
        let file_path = file_path.as_ref();
        if !file_path.is_file() {
            return Ok(PListLoader::new(None));
        }
        Ok(PListLoader::new(Some(std::fs::File::open(file_path)?)))
    }

    struct PListLoader<R> {
        reader: Option<R>,
    }

    impl<R: Read + Seek> PListLoader<R> {
        pub fn new(reader: Option<R>) -> Self {
            Self { reader }
        }
    }

    impl<R> Modifier for PListLoader<R>
    where
        R: Read + Seek,
    {
        fn modify(&mut self, config: &mut Config) -> Result<(), LoadError> {
            if let Some(reader) = self.reader.as_mut() {
                let buffer = BufReader::new(reader);
                if let Some(root) = plist::Value::from_reader(buffer)?.as_dictionary()
                    && let Some(skip_paths) = root
                        .get("SkipPaths")
                        .map(|value| value.as_array())
                        .flatten()
                {
                    for value in skip_paths.iter() {
                        if let Some(string) = value.as_string() {
                            let path = PathBuf::from(string);

                            config.ignored_directories.insert(path);
                        }
                    }
                }
            }
            Ok(())
        }
    }

    #[cfg(test)]
    mod tests {
        use std::{collections::BTreeSet, path::PathBuf};

        use crate::config::{Config, modifier};

        #[test]
        fn test_time_machine_plist() {
            use crate::config::Modifier;
            let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            let plist_test_file_path = manifest_dir.join("src/tests/test.plist");
            let mut modifier = modifier::time_machine_plist(plist_test_file_path).unwrap();
            let mut config = Config {
                search_directories: BTreeSet::new(),
                ignored_directories: BTreeSet::new(),
                whitelist_patterns: BTreeSet::new(),
                threads: 1,
                monitor_interval_secs: 0,
            };
            modifier.modify(&mut config).unwrap();
            assert_eq!(3, config.ignored_directories.len());
            assert!(
                config
                    .ignored_directories
                    .contains(&PathBuf::from("/Users/hey/Desktop"))
            );
            assert!(
                config
                    .ignored_directories
                    .contains(&PathBuf::from("/Users/hey/.colima"))
            );
            assert!(
                config
                    .ignored_directories
                    .contains(&PathBuf::from("/Users/hey/Downloads"))
            );
        }

        #[test]
        fn test_time_machine_plist_bad_plist() {
            use crate::config::Modifier;
            let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            let plist_test_file_path = manifest_dir.join("src/tests/bad.plist");
            let mut modifier = modifier::time_machine_plist(plist_test_file_path).unwrap();
            let mut config = Config {
                search_directories: BTreeSet::new(),
                ignored_directories: BTreeSet::new(),
                whitelist_patterns: BTreeSet::new(),
                threads: 1,
                monitor_interval_secs: 0,
            };
            let result = modifier.modify(&mut config);

            assert!(result.is_err());
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum LoadError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Plist(#[from] plist::Error),
    #[error(transparent)]
    Validation(#[from] ValidationError),
}

#[cfg(test)]
#[derive(thiserror::Error, Debug)]
pub enum SaveError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
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
            writeln!(f, " - {}", fail)?;
        }
        Ok(())
    }
}

#[derive(thiserror::Error, Debug)]
pub enum ValidationFail {
    #[error("File in search_directories: {0}")]
    FileInSearchPaths(PathBuf),
    #[error("File in ignored_directories: {0}")]
    FileInIgnoredDirectories(PathBuf),
    #[error("Not found {0}")]
    NotFound(PathBuf),
    #[error("No search directories")]
    NoSearchDirectories,
}

pub trait Modifier {
    fn modify(&mut self, config: &mut Config) -> Result<(), LoadError>;
}

impl Config {
    pub const DEFAULT_MONITOR_INTERVAL_SECS: u64 = 5;
    pub const DEFAULT_THREADS: usize = 4;

    pub fn load_or_create_file(file_path: impl AsRef<Path>) -> Result<Self, LoadError> {
        let file_path = file_path.as_ref();

        if file_path.is_file() {
            Self::load_from_file(file_path)
        } else {
            let mut default_config = Self::default();

            std::fs::create_dir_all(file_path.parent().unwrap())?;

            let file = File::create_new(file_path)?;

            serde_json::to_writer_pretty(file, &default_config)?;

            println!("Created configuration file '{}'", file_path.display());

            Self::expand(&mut default_config);

            Ok(default_config)
        }
    }

    pub fn load_from_file(file_path: impl AsRef<Path>) -> Result<Self, LoadError> {
        Self::load(File::open(file_path)?)
    }

    pub fn load(reader: impl Read) -> Result<Self, LoadError> {
        let reader = BufReader::new(reader);
        let mut config = serde_json::from_reader(reader)?;
        Self::expand(&mut config);
        Self::validate(&config)?;
        Ok(config)
    }

    #[cfg(test)]
    pub fn save_to_file(&self, file_path: impl AsRef<Path>) -> Result<(), SaveError> {
        let file = File::create(file_path)?;
        self.save(file)?;
        Ok(())
    }

    #[cfg(test)]
    pub fn save(&self, writer: impl std::io::Write) -> Result<(), SaveError> {
        serde_json::to_writer_pretty(writer, self)?;
        Ok(())
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

    pub fn reload_file(&mut self, file_path: impl AsRef<Path>) -> Result<(), LoadError> {
        self.reload(std::fs::File::open(file_path)?)?;

        Ok(())
    }

    pub fn reload(&mut self, reader: impl Read) -> Result<(), LoadError> {
        *self = Self::load(reader)?;
        Self::expand(self);
        Self::validate(self)?;
        Ok(())
    }

    pub fn modify(mut self, mut modifier: impl Modifier) -> Result<Self, LoadError> {
        modifier.modify(&mut self)?;
        Ok(self)
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
            monitor_interval_secs: Self::DEFAULT_MONITOR_INTERVAL_SECS,
        }
    }
}

#[cfg(test)]
mod tests {
    use temp_dir_builder::TempDirectoryBuilder;

    use crate::config::{Config, LoadError, Modifier};

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
  "monitor_interval_secs": 5 }
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
  "monitor_interval_secs": 5 }
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

        assert!(matches!(result, Err(LoadError::Json(_))));
    }

    #[test]
    fn test_search_paths_does_not_exist() {
        let json = r#"
{
"search_directories": ["/does_not_exist"],
"ignored_directories": [],
"whitelist_patterns": [],
"threads": 4,
"monitor_interval_secs": 5 }
"#;
        let result = Config::load(json.as_bytes());

        assert!(matches!(result, Err(LoadError::Validation(_))));
    }

    #[test]
    fn test_no_search_paths() {
        let json = r#"
{
"search_directories": [],
"ignored_directories": [],
"whitelist_patterns": [],
"threads": 4,
"monitor_interval_secs": 5 }
"#;
        let result = Config::load(json.as_bytes());

        assert!(matches!(result, Err(LoadError::Validation(_))));
    }

    #[test]
    fn test_reload() {
        let json = r#"
{ "search_directories": ["~"],
  "ignored_directories": [],
  "whitelist_patterns": [],
  "threads": 4,
  "monitor_interval_secs": 5 }
"#;
        let mut config = Config::load(json.as_bytes()).unwrap();
        let json = r#"
{ "search_directories": ["~"],
  "ignored_directories": ["~"],
  "whitelist_patterns": [],
  "threads": 4,
  "monitor_interval_secs": 5 }
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
  "monitor_interval_secs": 5 }
"#;
        let mut config = Config::load(json.as_bytes()).unwrap();
        let invalid_json = r#"
{ "search_directories": [],
  "ignored_directories": [],
  "whitelist_patterns": [],
  "threads": 4,
  "monitor_interval_secs": 5 }
"#;
        assert!(config.reload(invalid_json.as_bytes()).is_err());
        assert_eq!(1, config.search_directories.len());
    }

    struct TestModifier;

    impl Modifier for TestModifier {
        fn modify(&mut self, config: &mut Config) -> Result<(), LoadError> {
            config.threads += 1;
            Ok(())
        }
    }

    #[test]
    fn test_config_modifier() {
        let json = r#"
{
"search_directories": ["~"],
"ignored_directories": [],
"whitelist_patterns": [],
"threads": 4,
"monitor_interval_secs": 5 }
"#;
        let result = Config::load(json.as_bytes())
            .unwrap()
            .modify(TestModifier)
            .unwrap();
        assert!(result.threads == 5);
    }
}
