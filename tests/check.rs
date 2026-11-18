use std::process::Command;

use temp_dir_builder::TempDirectoryBuilder;

#[test]
fn test_check_prints_only_its_result_on_the_standard_output()
-> Result<(), Box<dyn std::error::Error>> {
    let mut builder = TempDirectoryBuilder::default();
    let file = builder.add_empty_file("file");
    let temp_dir = builder.build()?;
    let file_path = temp_dir.path_of(&file);

    let check = || {
        Command::new(env!("CARGO_BIN_EXE_tmignore-rs"))
            .arg("--config")
            .arg(temp_dir.join("config.json"))
            .arg("--cache")
            .arg(temp_dir.join("cache.db"))
            .arg("--legacy-config")
            .arg(temp_dir.join("legacy-config.json"))
            .arg("--legacy-cache")
            .arg(temp_dir.join("legacy-cache.json"))
            .arg("check")
            .arg(&file_path)
            .output()
    };

    check()?;

    let standard_output = String::from_utf8(check()?.stdout)?;

    assert_eq!(
        Some(file_path.to_string_lossy().as_ref()),
        standard_output.lines().next(),
        "the standard output must start with the checked path when the configuration file \
         already exists"
    );

    Ok(())
}
