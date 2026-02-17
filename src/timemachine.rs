use std::path::Path;

const XATTR_NAME: &str = "com.apple.metadata:com_apple_backup_excludeItem";
const XATTR_VALUE: &[u8] = b"com.apple.backupd";

pub fn add_exclusion(path: impl AsRef<Path>) -> Result<(), std::io::Error> {
    xattr::set(path.as_ref(), XATTR_NAME, XATTR_VALUE)?;

    Ok(())
}

pub fn remove_exclusion(path: impl AsRef<Path>) -> Result<(), std::io::Error> {
    xattr::remove(path.as_ref(), XATTR_NAME)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{path::Path, process::Command};

    use temp_dir_builder::TempDirectoryBuilder;

    use crate::timemachine::{add_exclusion, remove_exclusion};

    fn is_excluded_from_time_machine(path: impl AsRef<Path>) -> bool {
        let path = path.as_ref();

        let output = Command::new("/usr/bin/tmutil")
            .arg("isexcluded")
            .arg(path)
            .output();

        match output {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                stdout.contains("[Excluded]")
            }
            Err(_) => false,
        }
    }

    #[test]
    fn test_add_exclusion() {
        let temp_dir = TempDirectoryBuilder::default()
            .root_folder(std::env::current_dir().unwrap().join("temp_dir_for_testing_test_add_exclusion"))
            .add_empty_file("test.txt")
            .build()
            .unwrap();
        let test_file = temp_dir.path().join("test.txt");
        assert_eq!(false, is_excluded_from_time_machine(&test_file));
        add_exclusion(&test_file).unwrap();
        assert_eq!(true, is_excluded_from_time_machine(&test_file));
    }

    #[test]
    fn test_remove_exclusion() {
        let temp_dir = TempDirectoryBuilder::default()
            .root_folder(std::env::current_dir().unwrap().join("temp_dir_for_testing_test_remove_exclusion"))
            .add_empty_file("test.txt")
            .build()
            .unwrap();
        let test_file = temp_dir.path().join("test.txt");
        add_exclusion(&test_file).unwrap();
        assert_eq!(true, is_excluded_from_time_machine(&test_file));
        remove_exclusion(&test_file).unwrap();
        assert_eq!(false, is_excluded_from_time_machine(&test_file));
    }
}
