pub fn plural(word: &str, count: usize) -> String {
    let mut word = word.to_string();
    if count == 0 || count > 1 {
        if word.ends_with('y') && word.len() > 1 {
            let _ = word.pop();
            word.push_str("ies");
        } else {
            word.push('s');
        }
    }
    word
}

#[cfg(test)]
mod tests {
    #[rstest::rstest]
    #[case("thing", 1usize, "thing")]
    #[case("thing", 0usize, "things")]
    #[case("repository", 2usize, "repositories")]
    fn test_plural(#[case] input: &str, #[case] count: usize, #[case] expected: &str) {
        let result = super::plural(input, count);

        assert_eq!(result, expected);
    }
}
