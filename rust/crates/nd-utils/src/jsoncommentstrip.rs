//! JSON-comment stripping — port of `utils/jsoncommentstrip` (an `io.Reader`
//! in Go; here a byte-stream transformer). Removes single-line comments
//! (`//` to end of line) and block comments (`/* ... */`) while preserving
//! comment-like sequences inside JSON string values.

#[derive(Clone, Copy, PartialEq)]
enum State {
    Normal,
    InString,
    InStringEscape,
    MaybeComment,
    LineComment,
    BlockComment,
    MaybeBlockEnd,
}

/// Strips JavaScript-style comments from `input`, returning the cleaned JSON.
#[must_use]
pub fn strip(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut state = State::Normal;
    let mut pending: Vec<u8> = Vec::new();

    for (i, &b) in bytes.iter().enumerate() {
        match state {
            State::Normal => match b {
                b'"' => {
                    out.push(b);
                    state = State::InString;
                }
                b'/' => state = State::MaybeComment,
                _ => out.push(b),
            },
            State::InString => {
                out.push(b);
                match b {
                    b'\\' => state = State::InStringEscape,
                    b'"' => state = State::Normal,
                    _ => {}
                }
            }
            State::InStringEscape => {
                out.push(b);
                state = State::InString;
            }
            State::MaybeComment => match b {
                b'/' => state = State::LineComment,
                b'*' => state = State::BlockComment,
                _ => {
                    // The '/' was not a comment start; emit it and the current byte
                    out.push(b'/');
                    out.push(b);
                    state = State::Normal;
                }
            },
            State::LineComment => {
                if b == b'\n' || b == b'\r' {
                    out.push(b);
                    state = State::Normal;
                }
                // Otherwise, consume and discard
            }
            State::BlockComment => {
                if b == b'*' {
                    state = State::MaybeBlockEnd;
                }
                // Otherwise, consume and discard
            }
            State::MaybeBlockEnd => match b {
                b'/' => state = State::Normal,
                b'*' => {} // stay (consecutive *'s)
                _ => state = State::BlockComment,
            },
        }
        pending.clear();
        let _ = i;
    }

    // Emit the pending '/' if input ended right after a '/' (Go reader emits it
    // at EOF).
    if state == State::MaybeComment {
        out.push(b'/');
    }

    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(input: &str) -> String {
        strip(input)
    }

    // compact mirrors the Go test's json.Compact comparison: whitespace
    // outside JSON strings is insignificant.
    fn compact(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut in_string = false;
        let mut escaped = false;
        for c in s.chars() {
            if in_string {
                out.push(c);
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == '"' {
                    in_string = false;
                }
            } else if c == '"' {
                out.push(c);
                in_string = true;
            } else if !c.is_whitespace() {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn passes_through_json_without_comments_unchanged() {
        let input = r#"{"key": "value", "num": 42}"#;
        assert_eq!(read(input), input);
    }

    #[test]
    fn strips_single_line_comments() {
        let input = "{\n\t// this is a comment\n\t\"key\": \"value\"\n}";
        assert_eq!(compact(&read(input)), compact("{\n\t\"key\": \"value\"\n}"));
    }

    #[test]
    fn strips_single_line_comments_at_end_of_line() {
        let input = "{\n\t\"key\": \"value\" // inline comment\n}";
        assert_eq!(read(input), "{\n\t\"key\": \"value\" \n}");
    }

    #[test]
    fn strips_block_comments() {
        let input = r#"{/* comment */"key": "value"}"#;
        assert_eq!(read(input), r#"{"key": "value"}"#);
    }

    #[test]
    fn strips_multi_line_block_comments() {
        let input = "{\n\t/* this is\n\ta multi-line\n\tcomment */\n\t\"key\": \"value\"\n}";
        assert_eq!(compact(&read(input)), compact("{\n\t\"key\": \"value\"\n}"));
    }

    #[test]
    fn preserves_comment_sequences_inside_strings() {
        let input = r#"{"key": "value // not a comment"}"#;
        assert_eq!(read(input), input);
        let input = r#"{"key": "value /* not a comment */"}"#;
        assert_eq!(read(input), input);
    }

    #[test]
    fn handles_escaped_quotes_in_strings() {
        let input = r#"{"key": "val\"ue // not a comment"}"#;
        assert_eq!(read(input), input);
    }

    #[test]
    fn handles_slash_at_end_of_input_as_literal() {
        let input = r#"{"key": "value"}/"#;
        assert_eq!(read(input), input);
    }

    #[test]
    fn handles_star_inside_block_comment_not_followed_by_slash() {
        let input = r#"{/* a * b */"key": "value"}"#;
        assert_eq!(read(input), r#"{"key": "value"}"#);
    }

    #[test]
    fn handles_empty_input() {
        assert_eq!(read(""), "");
    }

    #[test]
    fn handles_mixed_comments_with_real_content() {
        let input = "{\n\t// line comment\n\t\"name\": \"test\", /* inline block */\n\t/* multi\n\t   line */\n\t\"value\": \"hello // world\",\n\t\"other\": 123 // trailing\n}";
        assert_eq!(
            compact(&read(input)),
            compact(
                "{\n\t\"name\": \"test\", \n\t\"value\": \"hello // world\",\n\t\"other\": 123\n}"
            )
        );
    }

    #[test]
    fn handles_consecutive_slashes_that_are_not_comments() {
        let input = r#"{"path": "/a/b"}"#;
        assert_eq!(read(input), input);
    }

    #[test]
    fn handles_block_comment_at_end_of_input() {
        let input = r#"{"key": "value"}/* comment */"#;
        assert_eq!(read(input), r#"{"key": "value"}"#);
    }

    #[test]
    fn strips_comment_with_windows_style_line_endings() {
        let input = "{\r\n// comment\r\n\"key\": \"value\"\r\n}";
        assert_eq!(compact(&read(input)), compact("{\"key\": \"value\"\r\n}"));
    }

    #[test]
    fn strips_line_comments_with_mixed_line_endings() {
        let input =
            "{\n\"one\": 1, // test //\n\"two\": 2, //test //\r\n\"string\": \"value\"\n//test\n}";
        let expected = "{\n\"one\": 1, \n\"two\": 2, \r\n\"string\": \"value\"\n\n}";
        assert_eq!(read(input), expected);
    }

    #[test]
    fn strips_line_comment_at_start_of_json() {
        let input = "{// woot\n\"one\": 1, // test //\n\"two\": 2, //test //\r\n\"string\": \"value\"\n//test\n}";
        let expected = "{\n\"one\": 1, \n\"two\": 2, \r\n\"string\": \"value\"\n\n}";
        assert_eq!(read(input), expected);
    }

    #[test]
    fn strips_block_comments_with_mixed_line_endings_inside() {
        let input = "{/* multi\nline\r\ncomment */\"one\":1}";
        assert_eq!(read(input), "{\"one\":1}");
    }

    #[test]
    fn handles_complex_mix_of_escapes_and_comments() {
        let input =
            "{/* multi\nline\r\ncomment */\"one\": \"a value \\\" // /*woot\"/* m\nl *///woot\r\n}";
        let expected = "{\"one\": \"a value \\\" // /*woot\"\r\n}";
        assert_eq!(read(input), expected);
    }
}
