use std::path::Path;

use serde::{Serialize, de::DeserializeOwned};

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub fn load_json_file<T: DeserializeOwned>(file_path: impl AsRef<Path>) -> Result<T, Error> {
    let file = std::fs::File::open(file_path)?;

    Ok(serde_json::from_reader(file)?)
}

pub fn save_json_file(file_path: impl AsRef<Path>, value: &impl Serialize) -> Result<(), Error> {
    let file = std::fs::File::create(file_path)?;

    serde_json::to_writer_pretty(file, value)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};
    use temp_dir_builder::TempDirectoryBuilder;

    use crate::json::{load_json_file, save_json_file};

    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct Test {
        value: i32,
    }

    #[test]
    fn test_json_save_load() {
        let test = Test { value: 123 };
        let temp_dir = TempDirectoryBuilder::default().build().unwrap();
        let file_path = temp_dir.path().join("test.json");
        save_json_file(&file_path, &test).unwrap();
        let loaded = load_json_file(&file_path).unwrap();
        assert_eq!(test, loaded);
    }
}
