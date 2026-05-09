use chrono::{DateTime, Utc};
use polyphony_core::RuntimeSnapshot;
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::{
    app::{AppState, DetailView},
    theme::Theme,
};

pub(crate) fn kv_line<'a>(label: &'static str, value: &str, theme: Theme) -> Line<'a> {
    Line::from(vec![
        Span::styled(format!("{label:<10}"), Style::default().fg(theme.muted)),
        Span::styled(value.to_string(), Style::default().fg(theme.foreground)),
    ])
}

/// Strip HTML tags from text, preserving content between tags.
pub(crate) fn strip_html_tags(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_tag = false;
    for ch in input.chars() {
        match ch {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {},
        }
    }
    out
}

/// Pick a color for a label based on common keywords.
pub(crate) fn label_color(label: &str, theme: Theme) -> Color {
    match label.to_ascii_lowercase().as_str() {
        "bug" | "defect" => theme.danger,
        "feature" | "enhancement" => theme.success,
        "documentation" | "docs" => theme.info,
        "good first issue" | "help wanted" => Color::Cyan,
        "priority" | "urgent" | "critical" => theme.warning,
        "wontfix" | "invalid" | "duplicate" => theme.muted,
        _ => theme.foreground,
    }
}

pub(crate) fn format_relative_time(dt: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let secs = now.signed_duration_since(dt).num_seconds().max(0) as u64;
    if secs < 60 {
        "just now".into()
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h", secs / 3600)
    } else if secs < 604800 {
        format!("{}d", secs / 86400)
    } else if secs < 2_592_000 {
        format!("{}w", secs / 604800)
    } else {
        format!("{}mo", secs / 2_592_000)
    }
}

pub(crate) fn render_separator(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    width: u16,
    theme: Theme,
) {
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "─".repeat(width as usize),
            Style::default().fg(theme.border),
        ))),
        area,
    );
}

/// Build a breadcrumb Line from the detail_stack entries and snapshot data.
pub(crate) fn build_breadcrumb<'a>(app: &AppState, snapshot: &RuntimeSnapshot) -> Line<'a> {
    let mut spans: Vec<Span<'a>> = Vec::new();
    let theme = app.theme;

    // Give each breadcrumb entry more room when there are fewer entries.
    let max_title = match app.detail_stack.len() {
        0 | 1 => 80,
        2 => 40,
        _ => 25,
    };

    for (i, view) in app.detail_stack.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" › ", Style::default().fg(theme.muted)));
        }
        match view {
            DetailView::InboxItem { item_id, .. } => {
                let title = snapshot
                    .inbox_items
                    .iter()
                    .find(|t| t.item_id == *item_id)
                    .map(|t| truncate_str(&t.title, max_title))
                    .unwrap_or_else(|| item_id.clone());
                spans.push(Span::styled(
                    title,
                    Style::default()
                        .fg(theme.highlight)
                        .add_modifier(Modifier::BOLD),
                ));
            },
            DetailView::Run { run_id, .. } => {
                let title = snapshot
                    .runs
                    .iter()
                    .find(|m| m.id == *run_id)
                    .map(|m| truncate_str(&m.title, max_title))
                    .unwrap_or_else(|| run_id.clone());
                spans.push(Span::styled(
                    title,
                    Style::default()
                        .fg(theme.highlight)
                        .add_modifier(Modifier::BOLD),
                ));
            },
            DetailView::Task { task_id, .. } => {
                let title = snapshot
                    .tasks
                    .iter()
                    .find(|t| t.id == *task_id)
                    .map(|t| truncate_str(&t.title, max_title))
                    .unwrap_or_else(|| task_id.clone());
                spans.push(Span::styled(
                    title,
                    Style::default()
                        .fg(theme.highlight)
                        .add_modifier(Modifier::BOLD),
                ));
            },
            DetailView::Agent { agent_index, .. } => {
                let label = if let Some(running) = snapshot.running.get(*agent_index) {
                    format!("{} ({})", running.agent_name, running.issue_identifier)
                } else if let Some(history) = snapshot
                    .agent_run_history
                    .get(agent_index.saturating_sub(snapshot.running.len()))
                {
                    format!("{} ({})", history.agent_name, history.issue_identifier)
                } else {
                    format!("Agent #{agent_index}")
                };
                spans.push(Span::styled(
                    label,
                    Style::default()
                        .fg(theme.highlight)
                        .add_modifier(Modifier::BOLD),
                ));
            },
            DetailView::Deliverable { run_id, .. } => {
                let title = snapshot
                    .runs
                    .iter()
                    .find(|m| m.id == *run_id)
                    .map(|m| truncate_str(&m.title, max_title))
                    .unwrap_or_else(|| run_id.clone());
                spans.push(Span::styled(
                    title,
                    Style::default()
                        .fg(theme.highlight)
                        .add_modifier(Modifier::BOLD),
                ));
            },
            DetailView::Repo { repo_id, .. } => {
                spans.push(Span::styled(
                    repo_id.clone(),
                    Style::default()
                        .fg(theme.highlight)
                        .add_modifier(Modifier::BOLD),
                ));
            },
            DetailView::Events { .. } => {
                spans.push(Span::styled(
                    "Events",
                    Style::default()
                        .fg(theme.highlight)
                        .add_modifier(Modifier::BOLD),
                ));
            },
            DetailView::LiveLog {
                agent_name,
                issue_identifier,
                ..
            } => {
                spans.push(Span::styled(
                    format!("Live: {agent_name} on {issue_identifier}"),
                    Style::default()
                        .fg(theme.highlight)
                        .add_modifier(Modifier::BOLD),
                ));
            },
        }
    }

    Line::from(spans)
}

fn truncate_str(s: &str, max_len: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max_len {
        s.to_string()
    } else {
        let keep = max_len.saturating_sub(1);
        let truncated: String = s.chars().take(keep).collect();
        format!("{truncated}…")
    }
}

/// Render a scroll position indicator ("line X/Y") at the bottom-right of the area.
/// Only shown when content exceeds the visible area.
pub(crate) fn render_scroll_indicator(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    scroll_pos: u16,
    total_lines: usize,
    visible_height: usize,
    theme: Theme,
) {
    if total_lines <= visible_height {
        return;
    }
    let label = format!(" {}/{} ", scroll_pos as usize + 1, total_lines);
    let label_len = label.len() as u16;
    if label_len >= area.width || area.height == 0 {
        return;
    }
    let indicator_area = Rect {
        x: area.x + area.width - label_len,
        y: area.y + area.height.saturating_sub(1),
        width: label_len,
        height: 1,
    };
    frame.render_widget(
        Paragraph::new(Span::styled(label, Style::default().fg(theme.muted))),
        indicator_area,
    );
}

pub(crate) fn render_stable_vertical_scrollbar(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    content_length: usize,
    viewport_length: usize,
    position: usize,
    arrows: bool,
) {
    if content_length <= viewport_length || area.width == 0 || area.height == 0 {
        return;
    }

    let x = area.x + area.width.saturating_sub(1);
    let arrow_cells = if arrows && area.height >= 2 {
        2
    } else {
        0
    };
    let track_y = area.y + u16::from(arrow_cells > 0);
    let track_height = area.height.saturating_sub(arrow_cells);
    if track_height == 0 {
        return;
    }

    let track_len = track_height as usize;
    let thumb_len = stable_thumb_len(content_length, viewport_length, track_len);
    let thumb_start = stable_thumb_start(content_length, track_len, thumb_len, position);

    let buffer = frame.buffer_mut();
    if arrow_cells > 0 {
        if let Some(cell) = buffer.cell_mut((x, area.y)) {
            cell.set_symbol("▲");
        }
        if let Some(cell) = buffer.cell_mut((x, area.y + area.height - 1)) {
            cell.set_symbol("▼");
        }
    }

    for index in 0..track_len {
        let symbol = if (thumb_start..thumb_start + thumb_len).contains(&index) {
            "█"
        } else {
            "║"
        };
        if let Some(cell) = buffer.cell_mut((x, track_y + index as u16)) {
            cell.set_symbol(symbol);
        }
    }
}

fn stable_thumb_len(content_length: usize, viewport_length: usize, track_len: usize) -> usize {
    let proportional = viewport_length
        .saturating_mul(track_len)
        .div_ceil(content_length);
    proportional.clamp(1, track_len)
}

fn stable_thumb_start(
    content_length: usize,
    track_len: usize,
    thumb_len: usize,
    position: usize,
) -> usize {
    let max_thumb_start = track_len.saturating_sub(thumb_len);
    let max_position = content_length.saturating_sub(1);
    if max_position == 0 {
        return 0;
    }
    let position = position.min(max_position);
    (position * max_thumb_start + max_position / 2) / max_position
}

#[cfg(test)]
mod scrollbar_tests {
    use super::*;

    #[test]
    fn stable_thumb_length_does_not_depend_on_position() {
        let content_length = 37;
        let viewport_length = 12;
        let track_len = 17;

        let thumb_len = stable_thumb_len(content_length, viewport_length, track_len);
        for position in 0..content_length {
            let thumb_start = stable_thumb_start(content_length, track_len, thumb_len, position);
            assert!(thumb_start + thumb_len <= track_len);
            assert_eq!(
                thumb_len,
                stable_thumb_len(content_length, viewport_length, track_len)
            );
        }
    }
}
