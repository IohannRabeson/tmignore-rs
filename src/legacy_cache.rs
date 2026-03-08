use std::path::PathBuf;

use serde::Deserialize;

#[derive(Deserialize)]
pub struct LegacyCache {
    pub paths: Vec<PathBuf>,
}
