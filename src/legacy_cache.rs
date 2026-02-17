use std::{io::BufReader, path::PathBuf};

use serde::Deserialize;

#[derive(Deserialize)]
pub struct LegacyCache {
    paths: Vec<PathBuf>,
}

impl LegacyCache {
    pub fn import() -> Result<Vec<PathBuf>, std::io::Error> {
        let cache_file_path = match dirs::cache_dir() {
            Some(cache_dir) => cache_dir.join("tmignore").join("cache.json"),
            None => return Ok(vec![]),
        };

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
