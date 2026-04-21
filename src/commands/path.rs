use std::io::Write;

use clap::Subcommand;

use crate::Cli;

#[derive(Subcommand, Clone, Copy)]
pub enum Paths {
    /// Path of the configuration file
    Config,
    /// Path of the cache file
    Cache,
    /// Path of the legacy configuration file
    LegacyConfig,
    /// Path of the legacy cache file
    LegacyCache,
}

pub fn execute(cli: &Cli, path: Paths, writer: &mut impl Write) -> anyhow::Result<()> {
    let file_path = match path {
        Paths::Config => cli.config.as_str(),
        Paths::Cache => cli.cache.as_str(),
        Paths::LegacyCache => cli.legacy_cache.as_str(),
        Paths::LegacyConfig => cli.legacy_config.as_str(),
    };
    writeln!(writer, "{file_path}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::Paths;

    use crate::Cli;

    #[rstest]
    #[case(Paths::Config, "/yo/config.json\n")]
    #[case(Paths::Cache, "/yo/cache.db\n")]
    #[case(Paths::LegacyCache, "/yo/legacy_cache.json\n")]
    #[case(Paths::LegacyConfig, "/yo/legacy_config.json\n")]
    fn test_execute(#[case] path: Paths, #[case] expected_output: &str) {
        let cli = Cli {
            command: crate::Commands::Path { path },
            verbose: false,
            config: String::from("/yo/config.json"),
            cache: String::from("/yo/cache.db"),
            legacy_config: String::from("/yo/legacy_config.json"),
            legacy_cache: String::from("/yo/legacy_cache.json"),
            help: None,
        };
        let mut buffer = vec![];
        super::execute(&cli, path, &mut buffer).unwrap();
        let output = String::from_utf8(buffer).unwrap();
        assert_eq!(expected_output, output);
    }
}
