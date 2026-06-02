use std::{
    collections::{BTreeSet, VecDeque},
    io::Write,
    path::PathBuf,
};

use chrono::{DateTime, Local};
use clap::Subcommand;

use crate::cache::Cache;

#[derive(Subcommand, Clone, Copy)]
pub enum Stats {
    /// Print the total size of all files excluded from the backup
    Size {
        /// Print a human readable size
        #[arg(short, long)]
        humanize: bool,
    },
    /// Print the date and time of the last cache update
    LastUpdate,
}

pub fn execute(cache: &Cache, writer: &mut impl Write, stat: Stats) -> anyhow::Result<()> {
    let paths = cache.paths()?;
    match stat {
        Stats::Size { humanize } => {
            let total = fetch_total_size(&paths)?;
            if humanize {
                writeln!(writer, "{}", bytesize::ByteSize::b(total))?;
            } else {
                writeln!(writer, "{total}")?;
            }
        }
        Stats::LastUpdate => {
            let last_update = cache.last_update()?;
            let local_time: DateTime<Local> = last_update.with_timezone(&Local);

            writeln!(writer, "{local_time}")?;
        }
    }
    Ok(())
}

fn fetch_total_size(paths: &[PathBuf]) -> anyhow::Result<u64> {
    let mut total = 0u64;
    let mut files_to_process: BTreeSet<PathBuf> = paths
        .iter()
        .filter(|path| path.is_file())
        .cloned()
        .collect();
    let mut dirs_to_process: VecDeque<PathBuf> =
        paths.iter().filter(|path| path.is_dir()).cloned().collect();

    while let Some(dir_to_process) = dirs_to_process.pop_front() {
        for entry in std::fs::read_dir(dir_to_process)?.flatten() {
            // Use the directory entry's own file type so symlinks are not
            // followed.
            // Previously we were using Path::is_file and Path::is_dir but
            // thoses are following symlinks.
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_file() {
                files_to_process.insert(entry.path());
            } else if file_type.is_dir() {
                dirs_to_process.push_back(entry.path());
            }
        }
    }

    for file in &files_to_process {
        if let Ok(metadata) = file.metadata() {
            total = total.saturating_add(metadata.len());
        }
    }

    Ok(total)
}

#[cfg(test)]
mod tests {
    use temp_dir_builder::TempDirectoryBuilder;

    use crate::{cache::Cache, commands::stats::Stats};

    #[test]
    fn test_execute() {
        let temp_dir = TempDirectoryBuilder::default()
            .add_text_file("a", "a")
            .add_text_file("dir/b", "bb")
            .add_text_file("c", "ccc")
            .build()
            .unwrap();
        let mut cache = Cache::open_in_memory().unwrap();
        let a = temp_dir.path().join("a");
        let dir = temp_dir.path().join("dir");
        cache.add_paths([a, dir].into_iter()).unwrap();
        let mut buffer = vec![];
        let stat_command = Stats::Size { humanize: false };
        super::execute(&cache, &mut buffer, stat_command).unwrap();
        let buffer_text = String::from_utf8(buffer).unwrap();
        let size: u64 = buffer_text.trim().parse().unwrap();

        assert_eq!(3, size);
    }

    #[test]
    fn test_fetch_total_size_does_not_follow_symlinks_outside_directory() {
        let temp_dir = TempDirectoryBuilder::default()
            .add_text_file("outside/big.txt", "0123456789")
            .add_directory("excluded")
            .build()
            .unwrap();
        let outside = temp_dir.path().join("outside");
        let excluded = temp_dir.path().join("excluded");
        // A symlink inside the excluded directory pointing at an unrelated tree.
        std::os::unix::fs::symlink(&outside, excluded.join("link")).unwrap();

        let total = super::fetch_total_size(&[excluded]).unwrap();

        assert_eq!(
            0, total,
            "fetch_total_size followed a symlink and counted files outside the excluded directory"
        );
    }
}
