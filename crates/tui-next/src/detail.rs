use polyphony_core::{InboxItemRow, RuntimeSnapshot, TaskRow};
use ratatui::{
    layout::{Constraint, Layout, Margin, Rect},
    style::Style,
    text::{Line, Span, Text},
    widgets::{Block, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Widget, Wrap},
};

use crate::{
    app::AppState,
    format::item_time_label,
    rows::display_rows,
    status::{state_color, state_icon},
    theme,
};

pub(crate) fn draw_detail(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    app: &mut AppState,
) {
    let Some(item) = selected_item(snapshot, app) else {
        Line::styled("No inbox item selected", Style::new().fg(theme::muted()))
            .render(area.inner(Margin::new(2, 1)), frame.buffer_mut());
        return;
    };

    let content_area = area.inner(Margin::new(2, 0));
    let show_sidebar = content_area.width >= 96;
    let (main_area, sidebar_area) = if show_sidebar {
        let [main, sidebar] = Layout::horizontal([Constraint::Min(48), Constraint::Length(38)])
            .spacing(3)
            .areas(content_area);
        (main, Some(sidebar))
    } else {
        (content_area, None)
    };

    draw_main(frame, main_area, snapshot, item, app);
    if let Some(sidebar) = sidebar_area {
        draw_sidebar(frame, sidebar, snapshot, item, app.tick);
    }
}

fn draw_main(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    item: &InboxItemRow,
    app: &mut AppState,
) {
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                format!("{} ", state_icon(&item.status)),
                Style::new().fg(state_color(&item.status)),
            ),
            Span::styled(&item.title, Style::new().fg(theme::text()).bold()),
        ]),
        Line::from(vec![
            Span::styled(
                format!("{}  ", item.identifier),
                Style::new().fg(theme::muted()),
            ),
            Span::styled(
                format!("{}  ", item.source),
                Style::new().fg(theme::primary()),
            ),
            Span::styled(&item.status, Style::new().fg(theme::muted())),
        ]),
        Line::raw(""),
    ];

    if let Some(description) = item
        .description
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        lines.push(Line::styled(
            "Description",
            Style::new().fg(theme::text()).bold(),
        ));
        lines.push(Line::styled(description, Style::new().fg(theme::muted())));
        lines.push(Line::raw(""));
    }

    let children = snapshot
        .inbox_items
        .iter()
        .filter(|candidate| candidate.parent_id.as_deref() == Some(item.item_id.as_str()))
        .collect::<Vec<_>>();
    if !children.is_empty() {
        lines.push(Line::styled(
            "Children",
            Style::new().fg(theme::text()).bold(),
        ));
        for child in children {
            lines.push(Line::from(vec![
                Span::styled("├── ", Style::new().fg(theme::muted())),
                Span::styled(
                    format!("{} ", state_icon(&child.status)),
                    Style::new().fg(state_color(&child.status)),
                ),
                Span::styled(&child.title, Style::new().fg(theme::muted())),
            ]));
        }
        lines.push(Line::raw(""));
    }

    let runs = snapshot
        .runs
        .iter()
        .filter(|run| run.issue_identifier.as_deref() == Some(item.identifier.as_str()))
        .collect::<Vec<_>>();
    if !runs.is_empty() {
        lines.push(Line::styled("Runs", Style::new().fg(theme::text()).bold()));
        for run in &runs {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{} ", run.status),
                    Style::new().fg(theme::primary()),
                ),
                Span::styled(&run.title, Style::new().fg(theme::muted())),
            ]));
            lines.push(Line::styled(
                format!(
                    "  tasks {}/{} · {}",
                    run.tasks_completed, run.task_count, run.kind
                ),
                Style::new().fg(theme::muted()),
            ));
        }
        lines.push(Line::raw(""));
    }

    let tasks = tasks_for_item(snapshot, item);
    if !tasks.is_empty() {
        lines.push(Line::styled("Tasks", Style::new().fg(theme::text()).bold()));
        for task in tasks {
            let icon = match task.status.to_string().as_str() {
                "completed" => "✓",
                "in_progress" => "●",
                "failed" => "⊘",
                _ => "·",
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{icon} "), Style::new().fg(theme::primary())),
                Span::styled(&task.title, Style::new().fg(theme::muted())),
            ]));
            let agent = task.agent_name.as_deref().unwrap_or("unassigned");
            lines.push(Line::styled(
                format!(
                    "  {} · {} turns · {} tokens",
                    agent, task.turns_completed, task.total_tokens
                ),
                Style::new().fg(theme::muted()),
            ));
        }
    }

    let rendered_height = lines
        .iter()
        .map(|line| line.width().div_ceil(area.width.max(1) as usize).max(1))
        .sum::<usize>();
    let max_scroll = rendered_height.saturating_sub(area.height as usize) as u16;
    app.detail_scroll = app.detail_scroll.min(max_scroll);

    Paragraph::new(Text::from(lines))
        .wrap(Wrap { trim: false })
        .scroll((app.detail_scroll, 0))
        .style(Style::new().bg(theme::bg()))
        .render(area, frame.buffer_mut());

    if max_scroll > 0 {
        render_scrollbar(
            frame,
            area,
            rendered_height,
            area.height as usize,
            app.detail_scroll as usize,
        );
    }
}

fn render_scrollbar(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    total: usize,
    visible_rows: usize,
    scroll: usize,
) {
    let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .begin_symbol(None)
        .end_symbol(None)
        .track_symbol(Some(" "))
        .track_style(Style::new().bg(theme::element()))
        .thumb_symbol(" ")
        .thumb_style(Style::new().bg(theme::border()));
    let scrollbar_area = Rect::new(area.x, area.y, area.width, area.height);
    let track_height = scrollbar_area.height;
    let max_scroll = total.saturating_sub(visible_rows);
    let thumb_height = ((u32::from(track_height) * visible_rows as u32) / total as u32)
        .max(1)
        .min(u32::from(track_height.saturating_sub(1)));
    let scroll_scale = u32::from(track_height).saturating_sub(thumb_height);
    let content_length = max_scroll.saturating_mul(scroll_scale as usize) + 1;
    let viewport_length = thumb_height.saturating_mul(max_scroll as u32).max(1);
    let mut state = ScrollbarState::new(content_length)
        .position(scroll.saturating_mul(scroll_scale as usize))
        .viewport_content_length(viewport_length as usize);
    frame.render_stateful_widget(scrollbar, scrollbar_area, &mut state);
}

fn draw_sidebar(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    item: &InboxItemRow,
    tick: u32,
) {
    frame.render_widget(
        Block::default().style(Style::new().bg(theme::element())),
        area,
    );
    let inner = area.inner(Margin::new(2, 1));
    let [content, footer] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(inner);
    let label_width = 10usize;
    let value_width = content.width.saturating_sub(label_width as u16 + 1) as usize;
    let mut metadata = vec![
        ("created", item_time_label(item)),
        ("id", item.identifier.clone()),
        ("source", item.source.clone()),
        ("state", item.status.clone()),
    ];
    if !item.repo_id.is_empty() {
        metadata.push(("repo", item.repo_id.clone()));
    }
    if let Some(priority) = item.priority {
        metadata.push(("priority", format!("P{priority}")));
    }
    if !item.labels.is_empty() {
        metadata.push(("labels", item.labels.join(", ")));
    }
    if let Some(author) = item.author.as_deref() {
        metadata.push(("author", author.to_string()));
    }
    metadata.push(("workspace", workspace_label(snapshot, item, tick)));
    metadata.sort_by_key(|(label, _)| *label);

    let mut lines = vec![Line::styled("Issue", Style::new().fg(theme::text()).bold())];
    lines.extend(
        metadata
            .iter()
            .map(|(label, value)| sidebar_kv(label, value, label_width, value_width)),
    );

    let tasks = tasks_for_item(snapshot, item);
    if !tasks.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled("Tasks", Style::new().fg(theme::text()).bold()));
        for task in tasks {
            let icon = match task.status.to_string().as_str() {
                "completed" => "✓",
                "in_progress" => "●",
                "failed" => "⊘",
                _ => "·",
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{icon} "), Style::new().fg(theme::primary())),
                Span::styled(&task.title, Style::new().fg(theme::muted())),
            ]));
            if let Some(agent) = task.agent_name.as_deref() {
                lines.push(Line::styled(
                    format!("  {agent}"),
                    Style::new().fg(theme::muted()),
                ));
            }
        }
    }

    let running = snapshot
        .running
        .iter()
        .filter(|run| run.issue_id == item.item_id || run.issue_identifier == item.identifier)
        .collect::<Vec<_>>();
    if !running.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "Running",
            Style::new().fg(theme::text()).bold(),
        ));
        for run in running {
            lines.push(Line::from(vec![
                Span::styled("● ", Style::new().fg(theme::primary())),
                Span::styled(&run.agent_name, Style::new().fg(theme::text())),
                Span::styled(format!(" {}", run.state), Style::new().fg(theme::muted())),
            ]));
            if let Some(message) = run.last_message.as_deref() {
                lines.push(Line::styled(message, Style::new().fg(theme::muted())));
            }
        }
    }

    lines.push(Line::raw(""));
    lines.push(Line::styled("Keys", Style::new().fg(theme::text()).bold()));
    lines.push(Line::from(vec![
        Span::styled("Esc", Style::new().fg(theme::text())),
        Span::styled(":back  ", Style::new().fg(theme::muted())),
        Span::styled("j/k", Style::new().fg(theme::text())),
        Span::styled(":scroll", Style::new().fg(theme::muted())),
    ]));

    Paragraph::new(Text::from(lines))
        .wrap(Wrap { trim: false })
        .style(Style::new().bg(theme::element()))
        .render(content, frame.buffer_mut());

    Line::from(vec![
        Span::styled("tracker:", Style::new().fg(theme::muted())),
        Span::styled(
            snapshot.tracker_kind.to_string(),
            Style::new().fg(theme::primary()),
        ),
    ])
    .render(footer, frame.buffer_mut());
}

fn workspace_label(snapshot: &RuntimeSnapshot, item: &InboxItemRow, tick: u32) -> String {
    if snapshot.loading.any_active() {
        return theme::BRAILLE_SPINNER[(tick / 4) as usize % theme::BRAILLE_SPINNER.len()]
            .to_string();
    }
    if item.has_workspace {
        "yes".to_string()
    } else {
        "no".to_string()
    }
}

fn sidebar_kv(label: &str, value: &str, label_width: usize, value_width: usize) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{label:<label_width$}"),
            Style::new().fg(theme::muted()),
        ),
        Span::styled(" ", Style::new().fg(theme::muted())),
        Span::styled(
            format!(
                "{:>value_width$}",
                crate::format::truncate(value, value_width)
            ),
            Style::new().fg(theme::text()),
        ),
    ])
}

fn selected_item<'a>(snapshot: &'a RuntimeSnapshot, app: &AppState) -> Option<&'a InboxItemRow> {
    display_rows(snapshot)
        .get(app.selected)
        .and_then(|row| snapshot.inbox_items.get(row.item_idx))
}

fn tasks_for_item<'a>(snapshot: &'a RuntimeSnapshot, item: &InboxItemRow) -> Vec<&'a TaskRow> {
    let run_ids = snapshot
        .runs
        .iter()
        .filter(|run| run.issue_identifier.as_deref() == Some(item.identifier.as_str()))
        .map(|run| &run.id)
        .collect::<Vec<_>>();
    snapshot
        .tasks
        .iter()
        .filter(|task| run_ids.contains(&&task.run_id))
        .collect()
}
