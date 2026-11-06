//! POSIX-like shell quoting — port of `utils/shellquote` (itself from
//! <https://github.com/kballard/go-shellquote>).

use std::fmt;

/// Errors returned by [`split`].
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum SplitError {
    /// A single-quoted string was not terminated.
    UnterminatedSingleQuote,
    /// A double-quoted string was not terminated.
    UnterminatedDoubleQuote,
    /// A backslash escape was not terminated.
    UnterminatedEscape,
}

impl fmt::Display for SplitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SplitError::UnterminatedSingleQuote => write!(f, "unterminated single-quoted string"),
            SplitError::UnterminatedDoubleQuote => write!(f, "unterminated double-quoted string"),
            SplitError::UnterminatedEscape => write!(f, "unterminated backslash-escape"),
        }
    }
}

impl std::error::Error for SplitError {}

#[derive(Clone, Copy, PartialEq)]
enum State {
    Unquoted,
    SingleQuoted,
    DoubleQuoted,
}

/// Splits a string into words following POSIX-like shell quoting rules.
/// It handles single quotes, double quotes, and backslash escapes.
///
/// # Errors
/// [`SplitError`] when a quote or escape is left unterminated.
pub fn split(input: &str) -> Result<Vec<String>, SplitError> {
    let input = input.as_bytes();
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut parse_state = State::Unquoted;

    let mut i = 0usize;
    while i < input.len() {
        let ch = input[i];

        match parse_state {
            State::Unquoted => match ch {
                b'\\' => {
                    if i + 1 >= input.len() {
                        return Err(SplitError::UnterminatedEscape);
                    }
                    if input[i + 1] == b'\n' {
                        // Line continuation: skip both backslash and newline
                        i += 2;
                        continue;
                    }
                    i += 1;
                    // SAFETY-free: input is valid UTF-8; copy byte-wise.
                    word.push_str(&String::from_utf8_lossy(&input[i..i + 1]));
                    in_word = true;
                }
                b'\'' => {
                    parse_state = State::SingleQuoted;
                    in_word = true;
                }
                b'"' => {
                    parse_state = State::DoubleQuoted;
                    in_word = true;
                }
                b' ' | b'\t' | b'\n' => {
                    if in_word {
                        words.push(std::mem::take(&mut word));
                        in_word = false;
                    }
                }
                _ => {
                    word.push_str(&String::from_utf8_lossy(&input[i..i + 1]));
                    in_word = true;
                }
            },

            State::SingleQuoted => {
                if ch == b'\'' {
                    parse_state = State::Unquoted;
                } else {
                    word.push_str(&String::from_utf8_lossy(&input[i..i + 1]));
                }
            }

            State::DoubleQuoted => match ch {
                b'"' => parse_state = State::Unquoted,
                b'\\' => {
                    if i + 1 >= input.len() {
                        return Err(SplitError::UnterminatedEscape);
                    }
                    let next = input[i + 1];
                    // In double quotes, backslash only escapes: $ ` " \n \
                    if matches!(next, b'$' | b'`' | b'"' | b'\n' | b'\\') {
                        if next == b'\n' {
                            // Line continuation: skip both backslash and newline
                            i += 2;
                            continue;
                        }
                        i += 1;
                        word.push_str(&String::from_utf8_lossy(&input[i..i + 1]));
                    } else {
                        // Backslash is literal for other characters
                        word.push('\\');
                    }
                }
                _ => {
                    word.push_str(&String::from_utf8_lossy(&input[i..i + 1]));
                }
            },
        }

        i += 1;
    }

    match parse_state {
        State::SingleQuoted => return Err(SplitError::UnterminatedSingleQuote),
        State::DoubleQuoted => return Err(SplitError::UnterminatedDoubleQuote),
        State::Unquoted => {}
    }

    if in_word {
        words.push(word);
    }

    Ok(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split_ok(input: &str) -> Vec<String> {
        split(input).expect("split must succeed")
    }

    #[test]
    fn splits_simple_space_separated_words() {
        assert_eq!(split_ok("a b c"), ["a", "b", "c"]);
    }

    #[test]
    fn handles_multiple_spaces_between_words() {
        assert_eq!(split_ok("a    b"), ["a", "b"]);
    }

    #[test]
    fn handles_single_quoted_strings() {
        assert_eq!(split_ok("'hello world'"), ["hello world"]);
    }

    #[test]
    fn handles_double_quoted_strings() {
        assert_eq!(split_ok("\"hello world\""), ["hello world"]);
    }

    #[test]
    fn handles_backslash_escapes_in_unquoted_mode() {
        assert_eq!(split_ok("hello\\ world"), ["hello world"]);
    }

    #[test]
    fn handles_escaped_quotes_inside_double_quotes() {
        assert_eq!(split_ok("\"hello \\\" world\""), ["hello \" world"]);
    }

    #[test]
    fn handles_mixed_quoting_in_a_single_argument() {
        assert_eq!(split_ok("he'llo wo'rld"), ["hello world"]);
    }

    #[test]
    fn returns_empty_slice_for_empty_input() {
        assert!(split_ok("").is_empty());
    }

    #[test]
    fn returns_empty_slice_for_whitespace_only_input() {
        assert!(split_ok("   ").is_empty());
    }

    #[test]
    fn returns_error_for_unterminated_single_quote() {
        assert_eq!(split("'hello"), Err(SplitError::UnterminatedSingleQuote));
    }

    #[test]
    fn returns_error_for_unterminated_double_quote() {
        assert_eq!(split("\"hello"), Err(SplitError::UnterminatedDoubleQuote));
    }

    #[test]
    fn returns_error_for_unterminated_escape() {
        assert_eq!(split("hello\\"), Err(SplitError::UnterminatedEscape));
    }

    #[test]
    fn handles_tabs_and_newlines_as_delimiters() {
        assert_eq!(split_ok("a\tb\nc"), ["a", "b", "c"]);
    }

    #[test]
    fn parses_the_default_mpv_command_template() {
        let words =
            split_ok("mpv --audio-device=%d --no-audio-display --pause %f --input-ipc-server=%s");
        assert_eq!(
            words,
            [
                "mpv",
                "--audio-device=%d",
                "--no-audio-display",
                "--pause",
                "%f",
                "--input-ipc-server=%s",
            ]
        );
    }

    #[test]
    fn preserves_spaces_in_quoted_paths() {
        assert_eq!(
            split_ok("--ao-pcm-file=\"/audio/my folder/file\""),
            ["--ao-pcm-file=/audio/my folder/file"]
        );
    }

    #[test]
    fn handles_backslash_in_double_quotes_for_special_chars() {
        assert_eq!(split_ok("\"hello\\\\world\""), ["hello\\world"]);
    }

    #[test]
    fn preserves_backslash_in_double_quotes_for_non_special_chars() {
        assert_eq!(split_ok("\"hello\\nworld\""), ["hello\\nworld"]);
    }

    #[test]
    fn handles_escaped_newline_in_double_quotes() {
        assert_eq!(split_ok("\"hello\\\nworld\""), ["helloworld"]);
    }

    // Cases from the original go-shellquote test suite
    #[test]
    fn handles_shell_glob_characters_as_literals() {
        assert_eq!(split_ok("glob* test?"), ["glob*", "test?"]);
    }

    #[test]
    fn handles_backslash_escaped_special_characters() {
        assert_eq!(
            split_ok("don\\'t you know the dewey decimal system\\?"),
            ["don't", "you", "know", "the", "dewey", "decimal", "system?"]
        );
    }

    #[test]
    fn handles_single_quote_escape_idiom() {
        // Shell idiom: end single-quote, escaped literal quote, start single-quote again
        assert_eq!(
            split_ok("'don'\\''t you know the dewey decimal system?'"),
            ["don't you know the dewey decimal system?"]
        );
    }

    #[test]
    fn handles_empty_string_argument_via_quotes() {
        assert_eq!(split_ok("one '' two"), ["one", "", "two"]);
    }

    #[test]
    fn handles_backslash_newline_joining_words_in_unquoted_mode() {
        assert_eq!(
            split_ok("text with\\\na backslash-escaped newline"),
            ["text", "witha", "backslash-escaped", "newline"]
        );
    }

    #[test]
    fn handles_quoted_newline_inside_double_quotes() {
        assert_eq!(
            split_ok("text \"with\na\" quoted newline"),
            ["text", "with\na", "quoted", "newline"]
        );
    }

    #[test]
    fn handles_complex_double_quoted_escapes_with_backslash_newline() {
        assert_eq!(
            split_ok("\"quoted\\d\\\\\\\" text with\\\na backslash-escaped newline\""),
            ["quoted\\d\\\" text witha backslash-escaped newline"]
        );
    }

    #[test]
    fn handles_backslash_newline_between_words() {
        assert_eq!(
            split_ok("text with an escaped \\\n newline in the middle"),
            ["text", "with", "an", "escaped", "newline", "in", "the", "middle"]
        );
    }

    #[test]
    fn handles_double_quoted_substring_concatenation() {
        assert_eq!(split_ok("foo\"bar\"baz"), ["foobarbaz"]);
    }

    #[test]
    fn returns_error_for_unterminated_quote_after_escape_idiom() {
        assert_eq!(
            split("'test'\\''ing"),
            Err(SplitError::UnterminatedSingleQuote)
        );
    }

    #[test]
    fn returns_error_for_unterminated_double_quote_with_single_quote_inside() {
        assert_eq!(split("\"foo'bar"), Err(SplitError::UnterminatedDoubleQuote));
    }

    #[test]
    fn returns_error_for_unterminated_escape_with_leading_whitespace() {
        assert_eq!(split("   \\"), Err(SplitError::UnterminatedEscape));
    }
}
