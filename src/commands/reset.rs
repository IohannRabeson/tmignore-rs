use crate::{Logger, cache::Cache};

use std::{collections::BTreeSet, error::Error};

pub fn execute(
    cache: &mut Cache,
    dry_run: bool,
    details: bool,
    logger: &mut Logger,
) -> Result<(), Box<dyn Error>> {
    let diff = cache.find_diff(&BTreeSet::new());

    super::apply_diff_and_print(&diff, dry_run, details, logger);

    if !dry_run {
        cache.reset([]);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::{Logger, cache::Cache};

    #[test]
    fn test_reset() {
        let temp_dir = crate::commands::tests::create_repository("test_reset");
        let mut cache = Cache::open_in_memory().unwrap();
        let config = crate::commands::tests::create_config(temp_dir.path());
        let dry_run = false;
        let mut logger = Logger::new(dry_run);
        crate::commands::run::execute(&config, &mut cache, dry_run, false, &mut logger).unwrap();
        super::execute(&mut cache, dry_run, false, &mut logger).unwrap();
        let paths = cache.paths();

        assert!(paths.is_empty());
    }
}
