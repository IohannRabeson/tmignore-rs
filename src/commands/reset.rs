use crate::{cache::Cache, commands::TimeMachine};

use std::collections::BTreeSet;

pub fn execute(cache: &mut Cache, dry_run: bool, details: bool) {
    let diff = cache.find_diff(&BTreeSet::new());

    super::apply_diff_and_print::<TimeMachine>(&diff, dry_run, details);

    if !dry_run {
        cache.reset([]);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::cache::Cache;

    #[test]
    fn test_reset() {
        let temp_dir = crate::commands::tests::create_repository(None::<PathBuf>);
        let mut cache = Cache::open_in_memory().unwrap();
        let config = crate::commands::tests::create_config(temp_dir.path());
        let dry_run = false;
        crate::commands::run::execute(&config, &mut cache, dry_run, false).unwrap();
        super::execute(&mut cache, dry_run, false);
        let paths = cache.paths();

        assert!(paths.is_empty());
    }
}
