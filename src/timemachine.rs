use core_foundation::base::OSStatus;
use core_foundation::base::TCFType;
use std::path::Path;
use std::path::PathBuf;

// I was not happy with the performance of tmutil when adding / removing files and folders to exclude.
// So I used otool to disassemble it, and I found it was in fact calling CSBackupSetItemExcluded.
// I also found why tmutil is so slow, it actually waits 1 second after calling CSBackupSetItemExcluded, and then after it calls
// _MDPerfCreateFileIndexingMarker.
// I used `otool -tvV /usr/bin/tmutil` and grep to search "exclude" and "exclusion", and I found among few other things:
// `000000010000ec30	bl	0x10002d41c ; symbol stub for: _CSBackupSetItemExcluded`.
// It catched my attention. After reading https://developer.apple.com/documentation/coreservices/1445043-csbackupsetitemexcluded
// I decided to try it, I tested to do few backups and it worked.
// After that I tried to find what was happening after this call, and after some lines I found the call to `_sleep`.
// mov	w0, #0x1
// 000000010000ee88	bl	0x10002d9fc ; symbol stub for: _sleep
#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    fn CSBackupSetItemExcluded(
        item: core_foundation::url::CFURLRef,
        exclude: u8,
        exclude_by_path: u8,
    ) -> OSStatus;
}

#[derive(thiserror::Error, Debug)]
enum ExcludePathError {
    #[error("OS Status {0}")]
    Os(OSStatus),
    #[error("Invalid UTF-8")]
    InvalidUtf8,
}

fn exclude_path(path: impl AsRef<Path>, exclude: bool) -> anyhow::Result<()> {
    use core_foundation::string::CFString;
    use core_foundation::url::CFURL;
    let path = path.as_ref();

    let url = CFURL::from_file_system_path(
        CFString::new(path.to_str().ok_or(ExcludePathError::InvalidUtf8)?),
        core_foundation::url::kCFURLPOSIXPathStyle,
        path.is_dir(),
    );

    let status =
        unsafe { CSBackupSetItemExcluded(url.as_concrete_TypeRef(), u8::from(exclude), 0) };

    if status == 0 {
        Ok(())
    } else {
        Err(ExcludePathError::Os(status).into())
    }
}

#[derive(PartialEq, Debug)]
pub struct Error {
    pub path: PathBuf,
    pub message: String,
}

pub fn add_exclusions<'a>(paths: impl Iterator<Item = &'a PathBuf>) -> Vec<Error> {
    let mut errors = vec![];

    for path in paths {
        if let Err(error) = exclude_path(path, true) {
            errors.push(Error {
                path: path.clone(),
                message: error.to_string(),
            });
        }
    }

    errors
}

pub fn remove_exclusions<'a>(paths: impl Iterator<Item = &'a PathBuf>) -> Vec<Error> {
    let mut errors = vec![];

    for path in paths {
        if let Err(error) = exclude_path(path, false) {
            errors.push(Error {
                path: path.clone(),
                message: error.to_string(),
            });
        }
    }

    errors
}

pub(crate) trait TmUtilsTrait {
    fn status() -> anyhow::Result<String>;
}

pub(crate) struct TmUtils;

impl TmUtilsTrait for TmUtils {
    fn status() -> anyhow::Result<String> {
        use anyhow::Context;

        let output = std::process::Command::new("/usr/bin/tmutil")
            .arg("status")
            .output()
            .context("Failed to execute tmutil status")?;

        status_from_output(&output)
    }
}

fn status_from_output(output: &std::process::Output) -> anyhow::Result<String> {
    if !output.status.success() {
        return Err(anyhow::anyhow!(
            "tmutil status exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

pub fn is_time_machine_running() -> anyhow::Result<bool> {
    is_time_machine_running_impl::<TmUtils>()
}

pub(crate) fn is_time_machine_running_impl<T: TmUtilsTrait>() -> anyhow::Result<bool> {
    Ok(parse_running(&T::status()?))
}

fn parse_running(tmutil_status_stdout: &str) -> bool {
    tmutil_status_stdout.contains("Running = 1")
}

#[cfg(test)]
pub(crate) mod tests {
    use std::{path::Path, process::Command};

    use rstest::rstest;
    use temp_dir_builder::TempDirectoryBuilder;

    use crate::timemachine::{TmUtilsTrait, add_exclusions, remove_exclusions};

    pub(crate) struct Running;
    impl TmUtilsTrait for Running {
        fn status() -> anyhow::Result<String> {
            Ok("Backup session status:\n{\n    Running = 1;\n}".to_string())
        }
    }

    pub(crate) struct NotRunning;
    impl TmUtilsTrait for NotRunning {
        fn status() -> anyhow::Result<String> {
            Ok("Backup session status:\n{\n    Running = 0;\n}".to_string())
        }
    }

    pub(crate) struct StatusError;
    impl TmUtilsTrait for StatusError {
        fn status() -> anyhow::Result<String> {
            Err(anyhow::anyhow!("can't determine the Time Machine status"))
        }
    }

    #[rstest]
    #[case("Backup session status:\n{\n    Running = 1;\n}", true)]
    #[case("Backup session status:\n{\n    Running = 0;\n}", false)]
    #[case("", false)]
    fn test_parse_running(#[case] stdout: &str, #[case] expected: bool) {
        assert_eq!(expected, super::parse_running(stdout));
    }

    #[test]
    fn test_is_time_machine_running_true() {
        assert_eq!(
            true,
            super::is_time_machine_running_impl::<Running>().unwrap()
        );
    }

    #[test]
    fn test_is_time_machine_running_false() {
        assert_eq!(
            false,
            super::is_time_machine_running_impl::<NotRunning>().unwrap()
        );
    }

    #[test]
    fn test_is_time_machine_running_error_propagates() {
        assert!(super::is_time_machine_running_impl::<StatusError>().is_err());
    }

    fn exited_with(code: i32) -> std::process::ExitStatus {
        use std::os::unix::process::ExitStatusExt;

        std::process::ExitStatus::from_raw(code << 8)
    }

    #[test]
    fn test_status_from_output_success() {
        let output = std::process::Output {
            status: exited_with(0),
            stdout: b"    Running = 1;".to_vec(),
            stderr: Vec::new(),
        };

        assert_eq!(
            "    Running = 1;",
            super::status_from_output(&output).unwrap()
        );
    }

    #[test]
    fn test_status_from_output_non_zero_exit() {
        let output = std::process::Output {
            status: exited_with(1),
            stdout: Vec::new(),
            stderr: b"boom".to_vec(),
        };

        assert!(super::status_from_output(&output).is_err());
    }

    // Be careful, this test is not very reliable even if it uses the offical way (tmutil isexcluded) to
    // know if a file is excluded. For example, as soon as you add the extended attribute
    // com.apple.metadata:com_apple_backup_excludeItem with any values, this test will return true, but
    // this is not enough to make an item excluded!
    // To really check if it's true it's needed to do a backup and verify using the Finder
    // by browsing the backup and checking if the items are present or not.
    pub(crate) fn is_excluded_from_time_machine(path: impl AsRef<Path>) -> bool {
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

    fn prepare_temp_dir(directory_name: &str) -> TempDirectoryBuilder {
        let path = std::env::current_dir().unwrap().join(directory_name);

        if path.is_dir() {
            std::fs::remove_dir_all(&path).unwrap();
        }

        TempDirectoryBuilder::default().root_folder(path)
    }

    #[test]
    fn test_add_exclusion() {
        let temp_dir = prepare_temp_dir("temp_dir_for_testing_test_add_exclusion")
            .add_empty_file("test.txt")
            .build()
            .unwrap();
        let test_file = temp_dir.path().join("test.txt");
        assert_eq!(false, is_excluded_from_time_machine(&test_file));
        assert!(add_exclusions([test_file.clone()].iter()).is_empty());
        assert_eq!(true, is_excluded_from_time_machine(&test_file));
    }

    #[test]
    fn test_remove_exclusion() {
        let temp_dir = prepare_temp_dir("temp_dir_for_testing_test_remove_exclusion")
            .add_empty_file("test.txt")
            .build()
            .unwrap();
        let test_file = temp_dir.path().join("test.txt");
        assert!(add_exclusions([test_file.clone()].iter()).is_empty());
        assert_eq!(true, is_excluded_from_time_machine(&test_file));
        assert!(remove_exclusions([test_file.clone()].iter()).is_empty());
        assert_eq!(false, is_excluded_from_time_machine(&test_file));
    }

    #[test]
    fn test_add_exclusion_directory() {
        let temp_dir = prepare_temp_dir("temp_dir_for_testing_test_add_exclusion_directory")
            .add_empty_file("dir/test.txt")
            .build()
            .unwrap();
        let test_dir = temp_dir.path().join("dir");
        let test_file = test_dir.join("test.txt");
        assert_eq!(false, is_excluded_from_time_machine(&test_dir));
        assert_eq!(false, is_excluded_from_time_machine(&test_file));
        assert!(add_exclusions([test_dir.clone()].iter()).is_empty());
        assert_eq!(true, is_excluded_from_time_machine(&test_dir));
        assert_eq!(true, is_excluded_from_time_machine(&test_file));
    }
}
