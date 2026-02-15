mod config;
mod find_repositories;

use std::error::Error;

use clap::{Parser, Subcommand};

use crate::config::Config;

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Run,
    List,
    Reset,
}

fn main() -> Result<(), Box<dyn Error>> {
    const CONFIG_FILE_PATH: &str = "~/.config/tmignore/config.json";
    let cli = Cli::parse();
    let config = Config::load_or_create_file(shellexpand::tilde(CONFIG_FILE_PATH).to_string())?;
    match cli.command {
        Commands::Run => run_command::execute(&config),
        Commands::List => list_command::execute(&config),
        Commands::Reset => reset_command::execute(&config),
    }
}

mod run_command {
    use std::error::Error;

    use crate::{config::Config, find_repositories::find_repositories};

    pub fn execute(config: &Config) -> Result<(), Box<dyn Error>> {
        let repositories = find_repositories(&config.search_directories);
        
        for repository in repositories {
            println!(" - {}", repository.display());
        }
        
        Ok(())
    }
}

mod list_command {
    use std::error::Error;

    use crate::config::Config;

    pub fn execute(config: &Config) -> Result<(), Box<dyn Error>> {
        println!("list");
        Ok(())
    }
}

mod reset_command {
    use crate::config::Config;

    use std::error::Error;

    pub fn execute(config: &Config) -> Result<(), Box<dyn Error>> {
        println!("reset");
        Ok(())
    }
}
