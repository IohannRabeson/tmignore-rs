use std::{collections::BTreeSet, path::PathBuf};

use serde::Deserialize;

#[derive(Deserialize)]
pub struct LegacyConfig {
    #[serde(rename = "searchPaths")]
    pub search_directories: BTreeSet<PathBuf>,
    #[serde(rename = "ignoredPaths")]
    pub ignored_directories: BTreeSet<PathBuf>,
    #[serde(rename = "whitelist")]
    pub whitelist_patterns: BTreeSet<String>,
}
