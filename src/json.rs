use std::path::Path;

use anyhow::Context;
use serde::{Serialize, de::DeserializeOwned};

pub fn load_json_file<T: DeserializeOwned>(file_path: impl AsRef<Path>) -> anyhow::Result<T> {
    let file_path = file_path.as_ref();
    let file = std::fs::File::open(file_path).with_context(|| file_path.display().to_string())?;

    serde_json::from_reader(file).with_context(|| file_path.display().to_string())
}

pub fn save_json_file(file_path: impl AsRef<Path>, value: &impl Serialize) -> anyhow::Result<()> {
    let file_path = file_path.as_ref();
    let tmp_path = file_path.with_extension("tmp");
    let file = std::fs::File::create(&tmp_path).with_context(|| tmp_path.display().to_string())?;

    if let Err(error) = serde_json::to_writer_pretty(file, value).with_context(|| tmp_path.display().to_string()) {
        // Since we are aborting on panic, using tempfile provides no benefits over manually removing the temporary file.
        let _ = std::fs::remove_file(&tmp_path);
        return Err(error);
    }

    std::fs::rename(&tmp_path, file_path).with_context(|| file_path.display().to_string())?;

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
