//! The closing account a turn ends with.
//!
//! The loop settles a turn when a round returns no tool calls, and the reply
//! is the text accumulated across its rounds. A model that narrates between
//! tool rounds ("Next I'll update the queue…") and then returns an empty
//! final round would leave that narration as the whole reply: a promise,
//! not an account of what was done. So an empty final round after tool
//! rounds is answered once with a request for the account — what was done,
//! what remains undone — and only a second empty round settles the turn,
//! with a statement that no account was given, so the reply never ends on
//! an announcement of work nobody can see.

/// The request put to the model, as a user-role message, when its final
/// round after tool rounds returns no text. One further round is run on it;
/// the tools stay offered, so a model that finds more to do may do it, and
/// the request is made once per turn.
pub const CLOSING_ACCOUNT_REQUEST: &str = "Your last round returned no text, so the turn has \
no closing account. End it with one: say what of the request was done and how you checked it, \
and name anything requested that remains undone. Do not announce further steps — this is the \
turn's final message.";

/// What the reply carries when the closing request itself returns no text.
pub const ENDED_WITHOUT_ACCOUNT: &str =
    "The turn ended without a closing account of what was done and what remains.";

/// Whether a round that returned no tool calls calls for the closing
/// request: its text is empty, at least one round came before it (which,
/// for the loop to have continued, was a tool round), and the request has
/// not already been made this turn.
pub fn closing_account_due(round_text: &str, round: usize, already_requested: bool) -> bool {
    round_text.trim().is_empty() && round > 1 && !already_requested
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_round_after_a_tool_round_is_due_a_request_once() {
        assert!(closing_account_due("", 2, false));
        assert!(closing_account_due("  \n", 5, false));
        assert!(!closing_account_due("", 2, true), "asked once per turn");
    }

    #[test]
    fn a_first_round_or_a_round_with_text_is_not() {
        assert!(
            !closing_account_due("", 1, false),
            "no tool round came before"
        );
        assert!(!closing_account_due("Done: the file is updated.", 3, false));
    }
}
