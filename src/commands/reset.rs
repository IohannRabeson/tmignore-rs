use crate::{Logger, cache::Cache};

    use std::{collections::BTreeSet, error::Error};

    pub fn execute(
        mut cache: Cache,
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