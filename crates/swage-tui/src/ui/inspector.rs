//! Context inspector panel — toggleable right panel showing the latest
//! context assembly report.
//!
//! Displays section names with token counts, total budget usage, and a
//! placeholder message when no assembly data is available yet.

use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Text},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::state::{ContextReport, ContextSection};

/// Width of the proportional fill bar in characters.
const BAR_WIDTH: usize = 10;

/// Build a proportional fill bar string.
///
/// `ratio` is the fraction to fill, clamped to [0.0, 1.0]. Returns a
/// BAR_WIDTH-character string of `█` (filled) and `░` (empty) wrapped in
/// square brackets, e.g. `[█████░░░░░]`.
fn make_bar(ratio: f64) -> String {
    let filled = (ratio.clamp(0.0, 1.0) * BAR_WIDTH as f64).round() as usize;
    let empty = BAR_WIDTH - filled;
    format!("[{}{}]", "█".repeat(filled), "░".repeat(empty))
}

/// Format a single section line: `"name: N tokens [bar]"`.
///
/// `budget_or_total` is used as the denominator for the bar ratio — pass
/// `budget_tokens` when known, otherwise pass `total_tokens` so the bar
/// shows the section's share of the whole context window.
fn format_section_line(section: &ContextSection, budget_or_total: u64) -> String {
    let ratio = if budget_or_total > 0 {
        section.token_count as f64 / budget_or_total as f64
    } else {
        0.0
    };
    format!(
        "{}: {} tokens {}",
        section.name,
        section.token_count,
        make_bar(ratio)
    )
}

/// Format the summary total line.
///
/// With budget: `"Total: N / B tokens (P%)"`.
/// Without budget: `"Total: N tokens"`.
fn format_total_line(total: u64, budget: Option<u64>) -> String {
    match budget {
        Some(b) if b > 0 => {
            let pct = (total as f64 / b as f64 * 100.0).round() as u64;
            format!("Total: {total} / {b} tokens ({pct}%)")
        }
        // Budget is known but zero, or entirely absent — skip the ratio.
        _ => format!("Total: {total} tokens"),
    }
}

/// Render the context inspector panel into `area`.
///
/// Shows a bordered "Context Inspector" block. When `context_report` is
/// `Some`, each section is listed with a proportional bar and the total
/// token count. When `None`, a placeholder prompt is shown instead.
pub fn render_inspector(frame: &mut Frame, area: Rect, context_report: Option<&ContextReport>) {
    let block = Block::default()
        .title("Context Inspector")
        .borders(Borders::ALL);

    // Reserve inner area before consuming `block` via render_widget.
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let text: Text = match context_report {
        None => Text::from("No context data yet — send a message to see the assembly report"),
        Some(report) => {
            // When budget is known, bars are relative to budget; otherwise
            // relative to the total so they still convey proportional size.
            let denominator = report.budget_tokens.unwrap_or(report.total_tokens);

            let mut lines: Vec<Line> = report
                .sections
                .iter()
                .map(|s| Line::from(format_section_line(s, denominator)))
                .collect();

            // Visual separator between sections and the summary.
            lines.push(Line::from(
                "─".repeat(inner.width.saturating_sub(2) as usize),
            ));

            lines.push(Line::from(format_total_line(
                report.total_tokens,
                report.budget_tokens,
            )));

            Text::from(lines)
        }
    };

    let paragraph = Paragraph::new(text).wrap(Wrap { trim: false });
    frame.render_widget(paragraph, inner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ContextSection;

    // --- make_bar ---

    #[test]
    fn make_bar_empty() {
        assert_eq!(make_bar(0.0), "[░░░░░░░░░░]");
    }

    #[test]
    fn make_bar_full() {
        assert_eq!(make_bar(1.0), "[██████████]");
    }

    #[test]
    fn make_bar_half() {
        assert_eq!(make_bar(0.5), "[█████░░░░░]");
    }

    #[test]
    fn make_bar_clamped_above_one() {
        // Over-budget sections should fill the bar but not overflow.
        assert_eq!(make_bar(2.0), "[██████████]");
    }

    #[test]
    fn make_bar_clamped_below_zero() {
        assert_eq!(make_bar(-1.0), "[░░░░░░░░░░]");
    }

    // --- format_section_line ---

    #[test]
    fn format_section_line_half_budget() {
        let section = ContextSection {
            name: "history".to_string(),
            token_count: 500,
        };
        let line = format_section_line(&section, 1000);
        assert!(
            line.starts_with("history: 500 tokens"),
            "expected prefix 'history: 500 tokens', got: {line}"
        );
        assert!(
            line.contains("[█████░░░░░]"),
            "expected half-filled bar in: {line}"
        );
    }

    #[test]
    fn format_section_line_full_budget() {
        let section = ContextSection {
            name: "system".to_string(),
            token_count: 100,
        };
        let line = format_section_line(&section, 100);
        assert!(
            line.contains("[██████████]"),
            "expected full bar in: {line}"
        );
    }

    #[test]
    fn format_section_line_zero_budget_or_total() {
        // When denominator is 0, the bar should be empty rather than panic.
        let section = ContextSection {
            name: "empty".to_string(),
            token_count: 0,
        };
        let line = format_section_line(&section, 0);
        assert!(
            line.contains("[░░░░░░░░░░]"),
            "expected empty bar in: {line}"
        );
    }

    // --- format_total_line ---

    #[test]
    fn format_total_line_with_budget() {
        assert_eq!(
            format_total_line(512, Some(1024)),
            "Total: 512 / 1024 tokens (50%)"
        );
    }

    #[test]
    fn format_total_line_without_budget() {
        assert_eq!(format_total_line(512, None), "Total: 512 tokens");
    }

    #[test]
    fn format_total_line_rounds_percentage() {
        // 1 / 3 ≈ 33%
        let line = format_total_line(1, Some(3));
        assert!(line.contains("33%"), "expected 33% in: {line}");
    }

    #[test]
    fn format_total_line_zero_budget() {
        // Zero budget should fall back to the no-budget format.
        assert_eq!(format_total_line(0, Some(0)), "Total: 0 tokens");
    }

    #[test]
    fn format_total_line_full_usage() {
        assert_eq!(
            format_total_line(4096, Some(4096)),
            "Total: 4096 / 4096 tokens (100%)"
        );
    }
}
