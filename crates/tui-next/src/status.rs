use ratatui::style::Color;

use crate::theme;

pub(crate) fn state_icon(status: &str) -> &'static str {
    match status.to_ascii_lowercase().as_str() {
        "open" | "in progress" | "started" | "in_progress" | "ready" => "●",
        "todo" | "unstarted" | "backlog" => "○",
        "debouncing" | "waiting_label" => "◷",
        "closed" | "done" | "completed" | "reviewed" | "already_fixed" => "✓",
        "cancelled" | "canceled" | "ignored_author" | "ignored_bot" | "ignored_label"
        | "ignored author" | "ignored bot" | "ignored label" => "⊘",
        "draft" => "◌",
        _ => "·",
    }
}

pub(crate) fn state_color(status: &str) -> Color {
    match status.to_ascii_lowercase().as_str() {
        "open" | "in progress" | "started" | "in_progress" | "ready" | "todo" | "unstarted"
        | "backlog" | "debouncing" | "waiting_label" => theme::primary(),
        "closed" | "done" | "completed" | "reviewed" | "already_fixed" => theme::done(),
        "draft" => theme::error(),
        _ => theme::muted(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_icons_match_known_polyphony_states() {
        assert_eq!(state_icon("open"), "●");
        assert_eq!(state_icon("todo"), "○");
        assert_eq!(state_icon("waiting_label"), "◷");
        assert_eq!(state_icon("done"), "✓");
        assert_eq!(state_icon("ignored_label"), "⊘");
        assert_eq!(state_icon("draft"), "◌");
    }
}
