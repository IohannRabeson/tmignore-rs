use std::{
    io::BufReader,
    path::{Path, PathBuf},
};

use serde::Deserialize;

#[derive(Deserialize)]
pub struct LegacyCache {
    paths: Vec<PathBuf>,
}

impl LegacyCache {
    pub fn import() -> Result<Vec<PathBuf>, std::io::Error> {
        // ~/Library/Caches/tmignore/cache.json
        let cache_file_path = match dirs::cache_dir() {
            Some(cache_dir) => cache_dir.join("tmignore").join("cache.json"),
            None => return Ok(vec![]),
        };

        Self::load_file(cache_file_path)
    }

    fn load_file(cache_file_path: impl AsRef<Path>) -> Result<Vec<PathBuf>, std::io::Error> {
        let cache_file_path = cache_file_path.as_ref();
        if !cache_file_path.is_file() {
            return Ok(vec![]);
        }
        println!(
            "Importing legacy cache file '{}'...",
            cache_file_path.display()
        );
        let file = std::fs::File::open(&cache_file_path)?;
        let cache: Self = serde_json::from_reader(BufReader::new(file))?;
        println!(
            "Imported successfully! You can now delete '{}' if you wish.",
            cache_file_path.display()
        );
        Ok(cache.paths)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use temp_dir_builder::TempDirectoryBuilder;

    use crate::legacy_cache::LegacyCache;

    #[test]
    fn test_import_legacy_cache() {
        let json = r#"{"paths":["a", "b"]}"#;
        let temp_dir = TempDirectoryBuilder::default()
            .add_text_file("cache.json", json)
            .build()
            .unwrap();
        let cache_file_path = temp_dir.path().join("cache.json");

        let cache = LegacyCache::load_file(cache_file_path).unwrap();

        assert_eq!(2, cache.len());
        assert_eq!(PathBuf::from("a"), cache[0]);
        assert_eq!(PathBuf::from("b"), cache[1]);
    }
}
