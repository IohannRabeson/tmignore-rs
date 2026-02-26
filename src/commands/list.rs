use std::error::Error;

    use crate::cache::Cache;

    pub fn execute(cache: Cache) -> Result<(), Box<dyn Error>> {
        for path in cache.paths() {
            println!("{}", path.display());
        }
        Ok(())
    }