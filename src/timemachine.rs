use std::path::PathBuf;

pub use crate::timemachine::tmutil::BreakingError;
pub use crate::timemachine::tmutil::Error;

const TMUTIL: &str = "/usr/bin/tmutil";
const ADDEXCLUSION: &str = "addexclusion";
const REMOVEEXCLUSION: &str = "removeexclusion";

fn max_command_line_size() -> usize {
    use nix::unistd::{SysconfVar, sysconf};

    match sysconf(SysconfVar::ARG_MAX) {
        Ok(Some(value)) => value as usize,
        _ => usize::MAX,
    }
}

pub fn add_exclusions<'a>(
    paths: impl Iterator<Item = &'a PathBuf>,
) -> Result<Vec<Error>, BreakingError> {
    let mut errors = vec![];
    let command_size = TMUTIL.len() + ADDEXCLUSION.len() + 2;
    let max_length = max_command_line_size();
    let (batches, discarded) = batches_paths(paths, max_length - command_size);

    for batch in &batches {
        let mut result = tmutil::call_tmutil(tmutil::TmutilVerb::AddExclusion, batch)?;

        errors.append(&mut result.errors);
    }

    errors.append(
        &mut discarded
            .into_iter()
            .map(|discarded_path| Error {
                path: discarded_path.clone(),
                message: String::from("Path too long"),
            })
            .collect(),
    );
    Ok(errors)
}

pub fn remove_exclusions<'a>(
    paths: impl Iterator<Item = &'a PathBuf>,
) -> Result<Vec<Error>, BreakingError> {
    let mut errors = vec![];
    let command_size = TMUTIL.len() + REMOVEEXCLUSION.len() + 2;
    let max_length = max_command_line_size();
    let (batches, discarded) = batches_paths(paths, max_length - command_size);

    for batch in &batches {
        let mut result = tmutil::call_tmutil(tmutil::TmutilVerb::RemoveExclusion, batch)?;

        errors.append(&mut result.errors);
    }

    errors.append(
        &mut discarded
            .into_iter()
            .map(|discarded_path| Error {
                path: discarded_path.clone(),
                message: String::from("Path too long"),
            })
            .collect(),
    );
    Ok(errors)
}

mod tmutil {
    use std::{
        path::{Path, PathBuf},
        string::FromUtf8Error,
    };

    use crate::timemachine::{ADDEXCLUSION, REMOVEEXCLUSION, TMUTIL};

    pub enum TmutilVerb {
        AddExclusion,
        RemoveExclusion,
    }

    #[derive(PartialEq, Debug)]
    pub struct Error {
        pub path: PathBuf,
        pub message: String,
    }

    pub struct TmutilResult {
        pub errors: Vec<Error>,
    }

    #[derive(thiserror::Error, Debug)]
    pub enum BreakingError {
        #[error(transparent)]
        Io(#[from] std::io::Error),
        #[error(transparent)]
        Utf8(#[from] FromUtf8Error),
    }

    pub fn call_tmutil(
        verb: TmutilVerb,
        paths: &[impl AsRef<Path>],
    ) -> Result<TmutilResult, BreakingError> {
        let output = std::process::Command::new(TMUTIL)
            .arg(match verb {
                TmutilVerb::AddExclusion => ADDEXCLUSION,
                TmutilVerb::RemoveExclusion => REMOVEEXCLUSION,
            })
            .args(paths.iter().map(|path| path.as_ref().to_owned()))
            .output()?;
        let stderr = String::from_utf8(output.stderr)?;
        let errors = parse_tmutil_errors(&stderr);

        Ok(TmutilResult { errors })
    }

    fn parse_tmutil_errors(stderr: &str) -> Vec<Error> {
        stderr
            .lines()
            .filter_map(|line| parse_tmutil_error(line))
            .collect()
    }

    fn parse_tmutil_error(line: &str) -> Option<Error> {
        const SEPARATOR: &str = ": ";
        let path_separator_index = line.find(SEPARATOR)?;
        let (path, message) = line.split_at(path_separator_index);
        let message = message.strip_prefix(SEPARATOR)?;
        Some(Error {
            path: PathBuf::from(path),
            message: message.to_string(),
        })
    }

    #[cfg(test)]
    mod tests {
        use std::path::PathBuf;

        use rstest::rstest;

        use crate::timemachine::tmutil::Error;

        #[rstest]
        #[case("/Users/hey/doesnt_exist: Error (100002) while attempting to change exclusion setting.", Some(Error {
        path: PathBuf::from("/Users/hey/doesnt_exist"),
        message: String::from("Error (100002) while attempting to change exclusion setting."),
    }))]
        fn test_parse_tmutil_error(#[case] input: &str, #[case] expected: Option<Error>) {
            let result = super::parse_tmutil_error(input);

            assert_eq!(expected, result);
        }
    }
}

fn batches_paths<'a>(
    paths: impl Iterator<Item = &'a PathBuf>,
    max_size_batch: usize,
) -> (Vec<Vec<&'a PathBuf>>, Vec<&'a PathBuf>) {
    let mut current_batch = vec![];
    let mut current_batch_size = 0usize;
    let mut batches = vec![];
    let mut discarded_paths = vec![];

    for path in paths {
        let path_size = path.as_os_str().len();

        if path_size > max_size_batch {
            discarded_paths.push(path);
            continue;
        }

        let next_batch_size =
            current_batch_size + path_size + if current_batch_size > 0 { 1 } else { 0 };
        if next_batch_size > max_size_batch {
            batches.push(current_batch.clone());
            current_batch.clear();
            current_batch.push(path);
            current_batch_size = path_size;
        } else {
            current_batch.push(path);
            if current_batch_size > 0 {
                // Count the space needed as separator
                current_batch_size += 1;
            }
            current_batch_size += path_size;
        }
    }
    if !current_batch.is_empty() {
        batches.push(current_batch.clone());
    }

    (batches, discarded_paths)
}

#[cfg(test)]
pub(crate) mod tests {
    use std::{
        path::{Path, PathBuf},
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
        add_exclusions([test_file.clone()].iter()).unwrap();
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
        add_exclusions([test_file.clone()].iter()).unwrap();
        assert_eq!(true, is_excluded_from_time_machine(&test_file));
        remove_exclusions([test_file.clone()].iter()).unwrap();
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
        add_exclusions([test_dir.clone()].iter()).unwrap();
        assert_eq!(true, is_excluded_from_time_machine(&test_dir));
        assert_eq!(true, is_excluded_from_time_machine(&test_file));
    }

    #[test]
    fn test_batches_paths_one_with_space() {
        let inputs = [PathBuf::from("a"), PathBuf::from("b")];
        let max_size = 2usize;
        let (batches, discarded) = super::batches_paths(inputs.iter(), max_size);
        assert_eq!(2, batches.len());
        assert_eq!(1, batches[0].len());
        assert_eq!(1, batches[1].len());
        assert!(discarded.is_empty());
    }

    #[test]
    fn test_batches_paths() {
        let inputs = [PathBuf::from("a"), PathBuf::from("b"), PathBuf::from("c")];
        let max_size = 3usize;
        let (batches, discarded) = super::batches_paths(inputs.iter(), max_size);
        assert_eq!(2, batches.len());
        assert_eq!(2, batches[0].len());
        assert_eq!(1, batches[1].len());
        assert!(discarded.is_empty());
    }

    #[test]
    fn test_batches_paths_path_too_long() {
        let too_long_path = PathBuf::from("abcd");
        let inputs = [
            PathBuf::from("a"),
            too_long_path.clone(),
            PathBuf::from("c"),
        ];
        let max_size = 3usize;
        let (batches, discarded) = super::batches_paths(inputs.iter(), max_size);

        assert_eq!(1, batches.len());
        assert_eq!(2, batches[0].len());
        assert_eq!(1, discarded.len());
        assert_eq!(&too_long_path, discarded[0]);
    }

    #[test]
    fn test_max_command_line_size() {
        let result = super::max_command_line_size();

        assert!(result > 0);
    }
}
