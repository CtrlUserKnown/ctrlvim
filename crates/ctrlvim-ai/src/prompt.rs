//! Fill-in-the-middle prompting for CodeGemma, and the cleanup its raw output
//! needs before it can be shown as ghost text.
//!
//! CodeGemma is trained for FIM with four control tokens, in *this* order —
//! prefix, suffix, then middle, which is not the order the text appears in:
//!
//! ```text
//! <|fim_prefix|>{code before the cursor}<|fim_suffix|>{code after}<|fim_middle|>
//! ```
//!
//! and generates until `<|file_separator|>` (its multi-file training separator)
//! or end-of-sequence. Everything here is pure string work with no model
//! attached, so the rules that decide what the user actually sees are testable
//! without a 2B-parameter download.

/// Marks the start of the code before the cursor.
pub const FIM_PREFIX: &str = "<|fim_prefix|>";
/// Marks the start of the code after the cursor.
pub const FIM_SUFFIX: &str = "<|fim_suffix|>";
/// Marks where generation begins.
pub const FIM_MIDDLE: &str = "<|fim_middle|>";
/// CodeGemma's between-files separator: reaching it means "the completion is
/// finished", not "here is more code".
pub const FILE_SEPARATOR: &str = "<|file_separator|>";

/// Every token that terminates a completion, whether or not the tokenizer
/// reports it as an EOS id. A model that starts a second FIM block, or echoes a
/// turn marker from its instruction-tuned sibling, has stopped being useful.
const STOP_MARKERS: &[&str] = &[
    FILE_SEPARATOR,
    FIM_PREFIX,
    FIM_SUFFIX,
    FIM_MIDDLE,
    "<|endoftext|>",
    "<eos>",
    "<start_of_turn>",
    "<end_of_turn>",
];

/// Build the FIM prompt for a cursor sitting between `prefix` and `suffix`.
pub fn fim(prefix: &str, suffix: &str) -> String {
    format!("{FIM_PREFIX}{prefix}{FIM_SUFFIX}{suffix}{FIM_MIDDLE}")
}

/// How the raw output is trimmed into something worth drawing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Trim {
    /// Hard cap on how many lines of ghost text to show. Long completions read
    /// as a wall of grey text and are almost never right past the first few
    /// lines.
    pub max_lines: usize,
}

impl Default for Trim {
    fn default() -> Self {
        Trim { max_lines: 8 }
    }
}

/// Turn a model's raw continuation into displayable ghost text.
///
/// `suffix` is the buffer text after the cursor, used to catch the model's
/// commonest failure: re-emitting code that is already on the other side of the
/// cursor, so accepting the suggestion would duplicate it.
///
/// Returns an empty string when there is nothing worth showing.
pub fn clean(raw: &str, suffix: &str, trim: Trim) -> String {
    let mut out = cut_at_stop_marker(raw);
    out = out.replace('\r', "");
    // Models routinely open with the newline that ends the prompt's last line.
    // Keep leading spaces (indentation is part of the completion) but drop a
    // leading break, which would otherwise render the ghost text a row below
    // where the cursor is.
    out = out.trim_start_matches('\n').to_string();
    out = drop_suffix_echo(&out, suffix);
    out = limit_lines(&out, trim.max_lines);
    // A completion of nothing but whitespace is noise: there is no visible
    // ghost text, but a `<Tab>` would still insert trailing blanks.
    if out.chars().all(char::is_whitespace) {
        return String::new();
    }
    out.trim_end_matches('\n').to_string()
}

/// Everything before the first stop marker.
fn cut_at_stop_marker(raw: &str) -> String {
    let end = STOP_MARKERS
        .iter()
        .filter_map(|m| raw.find(m))
        .min()
        .unwrap_or(raw.len());
    raw[..end].to_string()
}

/// Drop a tail that merely repeats what already follows the cursor.
///
/// FIM models are supposed to stop where the suffix picks up, and mostly do —
/// but when they don't, the duplication is glaring: a `}` closing a block that
/// is already closed, a re-typed `;`, or a whole second copy of the function
/// below. Accepting any of those inserts code the buffer already has.
///
/// The rule is "the longest tail of the completion that the suffix *starts*
/// with", compared with whitespace squeezed out so a difference in indentation
/// doesn't hide the repeat. Scanning from the front finds the longest such tail
/// first, which is the one to cut.
fn drop_suffix_echo(out: &str, suffix: &str) -> String {
    let suffix_key = squeeze(suffix);
    if suffix_key.is_empty() {
        return out.to_string();
    }
    for (start, _) in out.char_indices() {
        let key = squeeze(&out[start..]);
        if key.is_empty() {
            break;
        }
        if suffix_key.starts_with(&key) {
            return out[..start].to_string();
        }
    }
    out.to_string()
}

/// A whitespace-free view of `s`, for comparisons that shouldn't care about
/// indentation or line breaks.
fn squeeze(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Keep at most `max` lines.
fn limit_lines(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    match s.match_indices('\n').nth(max - 1) {
        Some((i, _)) => s[..i].to_string(),
        None => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fim_prompt_puts_the_suffix_before_the_middle_marker() {
        // The order matters and is *not* the order the text reads in: getting
        // it wrong produces plausible-looking but consistently worse output.
        let p = fim("fn add(a: i32", ") -> i32 { a + b }");
        assert_eq!(
            p,
            "<|fim_prefix|>fn add(a: i32<|fim_suffix|>) -> i32 { a + b }<|fim_middle|>"
        );
    }

    #[test]
    fn generation_stops_at_the_file_separator() {
        let out = clean("b: i32<|file_separator|>fn other() {}", "", Trim::default());
        assert_eq!(out, "b: i32");
    }

    #[test]
    fn a_second_fim_block_ends_the_completion() {
        let out = clean("done<|fim_prefix|>more", "", Trim::default());
        assert_eq!(out, "done");
    }

    #[test]
    fn turn_markers_from_the_instruction_tuned_sibling_are_cut() {
        let out = clean("x = 1<end_of_turn>chatter", "", Trim::default());
        assert_eq!(out, "x = 1");
    }

    #[test]
    fn a_leading_newline_is_dropped_but_indentation_is_kept() {
        // Ghost text has to start *at* the cursor; a leading break would draw
        // it a row too low.
        assert_eq!(clean("\n    body()", "", Trim::default()), "    body()");
    }

    #[test]
    fn a_tail_that_repeats_the_suffix_is_removed() {
        // The model completed the body and then re-typed the closing brace that
        // is already sitting after the cursor.
        let out = clean("\n    a + b\n}", "\n}\n", Trim::default());
        assert_eq!(out, "    a + b");
    }

    #[test]
    fn a_closing_delimiter_the_buffer_already_has_is_not_re_suggested() {
        // Cursor inside `foo(|)`: accepting `compute(x)` whole would leave
        // `foo(compute(x))`, so the trailing `)` the buffer already has is cut.
        assert_eq!(clean("compute(x)", ")", Trim::default()), "compute(x");
        // Same for a statement terminator sitting after the cursor.
        assert_eq!(clean("bar();", ";\n", Trim::default()), "bar()");
    }

    #[test]
    fn a_completion_that_is_entirely_the_suffix_is_nothing() {
        assert_eq!(clean("}\n", "}\n", Trim::default()), "");
    }

    #[test]
    fn output_is_capped_at_the_line_limit() {
        let raw = "1\n2\n3\n4\n5";
        assert_eq!(clean(raw, "", Trim { max_lines: 3 }), "1\n2\n3");
        assert_eq!(clean(raw, "", Trim { max_lines: 99 }), "1\n2\n3\n4\n5");
    }

    #[test]
    fn a_whitespace_only_completion_is_nothing() {
        // Otherwise `<Tab>` would silently insert trailing blanks against an
        // invisible ghost.
        assert_eq!(clean("   \n  \n", "", Trim::default()), "");
        assert_eq!(clean("", "", Trim::default()), "");
    }

    #[test]
    fn carriage_returns_never_reach_the_screen() {
        assert_eq!(clean("a\r\nb", "", Trim::default()), "a\nb");
    }
}
