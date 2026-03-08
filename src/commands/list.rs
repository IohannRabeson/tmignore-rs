use std::{error::Error, io::Write};

use crate::cache::Cache;

pub fn execute(
    cache: Cache,
    writer: &mut impl Write,
    separator: char,
) -> Result<(), Box<dyn Error>> {
    for path in cache.paths() {
        write!(writer, "{}{}", path.display(), separator)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::cache::Cache;

    #[test]
    fn test_execute() {
        let mut cache = Cache::open_in_memory().unwrap();
        cache.reset([PathBuf::from("a"), PathBuf::from("b"), PathBuf::from("c")]);
        let mut writer = Vec::new();

        super::execute(cache, &mut writer, '\n').unwrap();

        let text = String::from_utf8(writer).unwrap();
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(3, lines.len());
        assert_eq!("a", lines[0]);
        assert_eq!("b", lines[1]);
        assert_eq!("c", lines[2]);
    }
}
