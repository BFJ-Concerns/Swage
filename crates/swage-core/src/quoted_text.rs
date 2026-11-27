//! The quote around text a model is given to read and did not get from
//! whoever instructs it: an attached file's text, a fetched page, a pasted
//! log. The model takes everything inside the quote as that text and
//! everything outside it as the surrounding context's own words, so the
//! quote must end where the text ends, whatever the text holds.
//!
//! The delimiter is a fence: a line of backticks. A fixed fence fails on
//! text that carries the same fence — a Markdown file with a code block of
//! its own closes a three-backtick quote at its first block, and the rest of
//! the file reads as context. So the fence is chosen from the text, longer
//! than any run of backticks in it, and no line of the text can be mistaken
//! for the closing line. Nothing in the text is escaped or rewritten: the
//! model reads what the person chose.
//!
//! The context that carries a quote says, in its own standing words, where
//! a quote ends; `swage_context::materials` is the worked example of both
//! halves.

/// The length of the fence around text that holds no run of three or more
/// backticks: the fence a reader of Markdown already knows.
pub const SHORTEST_FENCE: usize = 3;

/// The label on the opening line. It says the quote is read as written,
/// whatever format the text is in, and tells the opening line from the
/// closing one.
pub const FENCE_LABEL: &str = "text";

/// `text` quoted for a model's context: an opening line, the text, and a
/// closing line, with no line break after the closing line.
///
/// - **The fence** is a run of backticks (U+0060) one longer than the
///   longest run of consecutive backticks anywhere in `text`, and never
///   shorter than [`SHORTEST_FENCE`]. Runs are counted wherever they fall,
///   not only at the start of a line, so the rule needs no parsing of the
///   text and holds for every text.
/// - **The opening line** is the fence followed directly by
///   [`FENCE_LABEL`].
/// - **The text** follows on the next line, byte for byte: not trimmed,
///   not escaped, its line endings kept. A line break is added after it
///   only when it does not already end in one, so the closing line always
///   stands on a line of its own.
/// - **The closing line** is the fence alone.
///
/// The fence depends on the text and nothing else, so the same text is
/// quoted the same way on every turn.
///
/// `````
/// use swage_core::quoted_text::verbatim_quote;
///
/// // No run of three backticks: the fence every Markdown reader knows.
/// assert_eq!(
///     verbatim_quote("Launch on Tuesday; `Alice` signs off."),
///     "```text\nLaunch on Tuesday; `Alice` signs off.\n```"
/// );
///
/// // A code block of its own: the fence outgrows it, and the text's final
/// // line break is not doubled.
/// assert_eq!(
///     verbatim_quote("Run it:\n\n```sh\njust verify\n```\n"),
///     "````text\nRun it:\n\n```sh\njust verify\n```\n````"
/// );
///
/// // A run in the middle of a line counts as much as one that starts it.
/// assert_eq!(
///     verbatim_quote("four ```` in a row"),
///     "`````text\nfour ```` in a row\n`````"
/// );
/// `````
pub fn verbatim_quote(text: &str) -> String {
    let longest_run = text
        .split(|character| character != '`')
        .map(str::len)
        .max()
        .unwrap_or(0);
    let fence = "`".repeat(SHORTEST_FENCE.max(longest_run + 1));
    let line_break = if text.ends_with('\n') { "" } else { "\n" };
    format!("{fence}{FENCE_LABEL}\n{text}{line_break}{fence}")
}
