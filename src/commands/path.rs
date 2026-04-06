use log::info;

use crate::{Cli, Paths};

pub fn execute(cli: &Cli, path: Paths) {
    match path {
        Paths::Config => info!("{}", cli.config),
        Paths::Cache => info!("{}", cli.cache),
    }
}
