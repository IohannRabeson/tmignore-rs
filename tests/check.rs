use std::{path::Path, process::Command};

use temp_dir_builder::{TempDirectory, TempDirectoryBuilder};

fn check_command(temp_dir: &TempDirectory, path: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tmignore-rs"));
    command
        .arg("--config")
        .arg(temp_dir.join("config.json"))
        .arg("--cache")
        .arg(temp_dir.join("cache.db"))
        .arg("--legacy-config")
        .arg(temp_dir.join("legacy-config.json"))
        .arg("--legacy-cache")
        .arg(temp_dir.join("legacy-cache.json"))
        .arg("check")
        .arg(path);
    command
}

#[test]
fn test_check_prints_only_its_result_on_the_standard_output()
-> Result<(), Box<dyn std::error::Error>> {
    let mut builder = TempDirectoryBuilder::default();
    let file = builder.add_empty_file("file");
    let temp_dir = builder.build()?;
    let file_path = temp_dir.path_of(&file);

    for run in ["creates", "loads"] {
        let standard_output =
            String::from_utf8(check_command(&temp_dir, &file_path).output()?.stdout)?;

        assert_eq!(
            Some(file_path.to_string_lossy().as_ref()),
            standard_output.lines().next(),
            "the standard output must start with the checked path when the run {run} the \
             configuration file"
        );
    }

    Ok(())
}

#[test]
fn test_check_exits_with_a_failure_status_when_a_path_cannot_be_read()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = TempDirectoryBuilder::default().build()?;
    let status = check_command(&temp_dir, &temp_dir.join("missing")).status()?;

    assert!(
        !status.success(),
        "a path could not be read, so the exit status must tell the command failed"
    );

    Ok(())
}
