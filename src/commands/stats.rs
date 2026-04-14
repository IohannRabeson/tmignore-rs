use std::{collections::{BTreeSet, VecDeque}, io::Write, path::PathBuf};

use chrono::{DateTime, Local};
use clap::Subcommand;

use crate::cache::Cache;

#[derive(Subcommand, Clone, Copy)]
pub enum Stats {
    /// Print the total size of all files excluded from the backup
    Size {
        /// Print a human readable size
        #[arg(short, long)]
        humanize: bool
    },
    /// Print the date and time of the last cache update 
    LastUpdate,
}

pub fn execute(cache: &Cache, writer: &mut impl Write, stat: Stats) -> anyhow::Result<()> {
    let paths = cache.paths();
    match stat {
        Stats::Size { humanize } => {
            let total = fetch_total_size(&paths)?;
            if humanize {
                writeln!(writer, "{}", bytesize::ByteSize::b(total))?;
            }
            else {
                writeln!(writer, "{total}")?;
            }
        },
        Stats::LastUpdate => {
            let last_update = cache.last_update();
            let local_time: DateTime<Local> = last_update.with_timezone(&Local);
            
            writeln!(writer, "{local_time}")?;
        },
    }
    Ok(())
}

fn fetch_total_size(paths: &[PathBuf]) -> anyhow::Result<u64> {
    let mut total = 0u64;
    let mut files_to_process: BTreeSet<PathBuf> = paths.iter().filter(|path|path.is_file()).cloned().collect();
    let mut dirs_to_process: VecDeque<PathBuf> = paths.iter().filter(|path|path.is_dir()).cloned().collect();

    while let Some(dir_to_process) = dirs_to_process.pop_front() {       
        for entry in std::fs::read_dir(dir_to_process)?.flatten() {
            let path = entry.path();
            if path.is_file() {
                files_to_process.insert(path.clone());
            }
            else if path.is_dir() {
                dirs_to_process.push_back(path.clone());
            }
        }
    }

    for file in &files_to_process {
        if let Ok(metadata) = file.metadata() {
            total += metadata.len();
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
        cache.add_paths([a, dir].into_iter());
        let mut buffer = vec![];
        let stat_command = Stats::Size { humanize: false };
        super::execute(&cache, &mut buffer, stat_command).unwrap();
        let buffer_text = String::from_utf8(buffer).unwrap();
        let size: u64 = buffer_text.trim().parse().unwrap();

        assert_eq!(3, size);
    }
}