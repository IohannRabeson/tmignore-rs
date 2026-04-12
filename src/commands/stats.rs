use std::{collections::{BTreeSet, VecDeque}, io::Write, path::PathBuf};

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
                writeln!(writer, "{}", total)?;
            }
        },
    }
    Ok(())
}

fn fetch_total_size(paths: &[PathBuf]) -> anyhow::Result<u64> {
    let mut total = 0u64;
    let mut files_to_process = BTreeSet::from_iter(paths.iter().filter(|path|path.is_file()).cloned());
    let mut dirs_to_process: VecDeque<PathBuf> = VecDeque::from_iter(paths.iter().filter(|path|path.is_dir()).cloned());

    while let Some(dir_to_process) = dirs_to_process.pop_front() {       
        for entry in std::fs::read_dir(dir_to_process)? {
            if let Ok(entry) = entry {
                let path = entry.path();
                if path.is_file() {
                    files_to_process.insert(path.to_path_buf());
                }
                else if path.is_dir() {
                    dirs_to_process.push_back(path.to_path_buf());
                }
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

