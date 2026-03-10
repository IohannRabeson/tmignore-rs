use std::path::Path;
use std::path::PathBuf;
use core_foundation::base::TCFType;
use core_foundation::base::OSStatus;

// I was not happy with the performance of tmutil when adding / removing files and folders to exclude.
// So I used otool to disassemble it, and I found it was in fact calling CSBackupSetItemExcluded.
// I also found why tmutil is so slow, it actually waits 1 second after calling CSBackupSetItemExcluded, and then after it calls
// _MDPerfCreateFileIndexingMarker.
// I used `otool -tvV /usr/bin/tmutil` and then I used grep to search "exclude" and "exclusion", and I found:
// `000000010000ec30	bl	0x10002d41c ; symbol stub for: _CSBackupSetItemExcluded`.
// It catched my attention, and I found https://developer.apple.com/documentation/coreservices/1445043-csbackupsetitemexcluded
// and at this point I decided to try it, I tested to do backups and it worked.
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

fn exclude_path(path: impl AsRef<Path>, exclude: bool) -> Result<(), ExcludePathError> {
    use core_foundation::url::CFURL;
    use core_foundation::string::CFString;
    let path = path.as_ref();

    let url = CFURL::from_file_system_path(
        CFString::new(path.to_str().ok_or(ExcludePathError::InvalidUtf8)?),
        core_foundation::url::kCFURLPOSIXPathStyle,
        path.is_dir(),
    );

    let status = unsafe {
        CSBackupSetItemExcluded(url.as_concrete_TypeRef(), if exclude { 1 } else { 0 }, 0)
    };

    if status == 0 { Ok(()) } else { Err(ExcludePathError::Os(status)) }
}

#[derive(PartialEq, Debug)]
pub struct Error {
    pub path: PathBuf,
    pub message: String,
}

pub fn add_exclusions<'a>(
    paths: impl Iterator<Item = &'a PathBuf>,
) -> Vec<Error> {
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

pub fn remove_exclusions<'a>(
    paths: impl Iterator<Item = &'a PathBuf>,
) -> Vec<Error> {
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

#[cfg(test)]
pub(crate) mod tests {
    use std::{
        path::Path,
        process::Command,
    };

    use temp_dir_builder::TempDirectoryBuilder;

    use crate::timemachine::{add_exclusions, remove_exclusions};

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

    #[test]
    fn test_add_exclusion() {
        let temp_dir = TempDirectoryBuilder::default()
            .root_folder(
                std::env::current_dir()
                    .unwrap()
                    .join("temp_dir_for_testing_test_add_exclusion"),
            )
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
        let temp_dir = TempDirectoryBuilder::default()
            .root_folder(
                std::env::current_dir()
                    .unwrap()
                    .join("temp_dir_for_testing_test_remove_exclusion"),
            )
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
        let temp_dir = TempDirectoryBuilder::default()
            .root_folder(
                std::env::current_dir()
                    .unwrap()
                    .join("temp_dir_for_testing_test_add_exclusion_directory"),
            )
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
