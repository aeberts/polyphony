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
    rows::display_rows_matching,
    status::{state_color, state_icon},
    theme, tracker,
    widgets::LeftRailPanel,
};

const CHILDREN_COLLAPSED_LIMIT: usize = 8;

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
    app.children_expand_rect = Rect::default();

    let mut panels = Vec::new();
    panels.push(LeftRailPanel::new(vec![
        Line::from(vec![
            Span::styled(
                format!("{} ", state_icon(&item.status)),
                Style::new().fg(state_color(&item.status)),
            ),
            Span::styled(item.title.clone(), Style::new().fg(theme::text()).bold()),
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
            Span::styled(item.status.clone(), Style::new().fg(theme::muted())),
        ]),
    ]));

    let description = item
        .description
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("No description.");
    let mut description_lines = vec![Line::styled(
        "Description",
        Style::new().fg(theme::text()).bold(),
    )];
    description_lines.extend(
        description
            .lines()
            .map(|line| Line::styled(line.to_string(), Style::new().fg(theme::muted()))),
    );
    panels.push(LeftRailPanel::new(description_lines));

    let children = children_for_item(snapshot, item);
    let children_expandable = children.len() > CHILDREN_COLLAPSED_LIMIT;
    let mut block = vec![Line::styled(
        "Children",
        Style::new().fg(theme::text()).bold(),
    )];
    if children.is_empty() {
        block.push(Line::styled(
            "No children.",
            Style::new().fg(theme::muted()),
        ));
    } else {
        let last_idx = children.len().saturating_sub(1);
        let visible_children = match (children_expandable, app.children_expanded) {
            (true, false) => CHILDREN_COLLAPSED_LIMIT,
            _ => children.len(),
        };
        for (idx, child) in children.iter().take(visible_children).enumerate() {
            let connector = match (
                children_expandable && !app.children_expanded,
                idx == last_idx,
            ) {
                (false, true) => "└── ",
                _ => "├── ",
            };
            block.push(Line::from(vec![
                Span::styled(connector, Style::new().fg(theme::muted())),
                Span::styled(
                    format!("{} ", state_icon(&child.status)),
                    Style::new().fg(state_color(&child.status)),
                ),
                Span::styled(child.title.clone(), Style::new().fg(theme::muted())),
            ]));
        }
        match (children_expandable, app.children_expanded) {
            (true, false) => {
                block.push(Line::styled("…", Style::new().fg(theme::muted())));
                block.push(Line::styled(
                    "Click to expand",
                    Style::new().fg(theme::secondary()),
                ));
            },
            (true, true) => {
                block.push(Line::styled(
                    "Click to collapse",
                    Style::new().fg(theme::secondary()),
                ));
            },
            (false, _) => {},
        }
    }
    let children_height = block.len().saturating_add(2) as u16;
    let children_panel_index = panels.len();
    panels.push(
        LeftRailPanel::new(block)
            .max_height(children_height)
            .bg(theme::element()),
    );

    let runs = snapshot
        .runs
        .iter()
        .filter(|run| run.issue_identifier.as_deref() == Some(item.identifier.as_str()))
        .collect::<Vec<_>>();
    if !runs.is_empty() {
        let mut block = vec![Line::styled("Runs", Style::new().fg(theme::text()).bold())];
        for run in &runs {
            block.push(Line::from(vec![
                Span::styled(
                    format!("{} ", run.status),
                    Style::new().fg(theme::primary()),
                ),
                Span::styled(run.title.clone(), Style::new().fg(theme::muted())),
            ]));
            block.push(Line::styled(
                format!(
                    "  tasks {}/{} · {}",
                    run.tasks_completed, run.task_count, run.kind
                ),
                Style::new().fg(theme::muted()),
            ));
        }
        panels.push(LeftRailPanel::new(block));
    }

    let tasks = tasks_for_item(snapshot, item);
    let mut block = vec![Line::styled("Tasks", Style::new().fg(theme::text()).bold())];
    if tasks.is_empty() {
        block.push(Line::styled("No tasks.", Style::new().fg(theme::muted())));
    } else {
        for task in tasks {
            let icon = match task.status.to_string().as_str() {
                "completed" => "✓",
                "in_progress" => "●",
                "failed" => "⊘",
                _ => "·",
            };
            block.push(Line::from(vec![
                Span::styled(format!("{icon} "), Style::new().fg(theme::primary())),
                Span::styled(task.title.clone(), Style::new().fg(theme::muted())),
            ]));
            let agent = task.agent_name.as_deref().unwrap_or("unassigned");
            block.push(Line::styled(
                format!(
                    "  {} · {} turns · {} tokens",
                    agent, task.turns_completed, task.total_tokens
                ),
                Style::new().fg(theme::muted()),
            ));
        }
    }
    panels.push(LeftRailPanel::new(block));

    let panel_width = area.width.saturating_sub(1);
    let rendered_height = panels
        .iter()
        .map(|panel| panel.visible_height(panel_width, panel_max_height(area.height)))
        .sum::<u16>()
        .saturating_add(panels.len().saturating_sub(1) as u16) as usize;
    let max_scroll = rendered_height.saturating_sub(area.height as usize) as u16;
    app.detail_scroll = app.detail_scroll.min(max_scroll);

    let rendered_areas = render_panels(
        frame,
        area,
        &panels,
        app.detail_scroll,
        match children_expandable {
            true => Some(children_panel_index),
            false => None,
        },
        app.mouse_pos,
    );
    if children_expandable {
        app.children_expand_rect = rendered_areas
            .get(children_panel_index)
            .copied()
            .flatten()
            .unwrap_or_default();
    }

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

fn render_panels(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    panels: &[LeftRailPanel],
    scroll: u16,
    hover_panel: Option<usize>,
    mouse_pos: Option<(u16, u16)>,
) -> Vec<Option<Rect>> {
    let mut rendered_areas = vec![None; panels.len()];
    let mut y_cursor = -(i32::from(scroll));
    let panel_width = area.width.saturating_sub(3);
    let max_panel_height = panel_max_height(area.height);
    for (idx, panel) in panels.iter().enumerate() {
        let height = panel.visible_height(panel_width, max_panel_height);
        let top = y_cursor;

        if top >= 0 && top < i32::from(area.height) {
            let visible_height = height.min(area.height.saturating_sub(top as u16));
            let panel_area = Rect::new(
                area.x,
                area.y + top as u16,
                area.width.saturating_sub(3),
                visible_height,
            );
            let hovered = hover_panel == Some(idx)
                && mouse_pos.is_some_and(|pos| panel_area.contains(pos.into()));
            let bg = match hovered {
                true => theme::element_hover(),
                false => theme::element(),
            };
            panel.render_clipped_with_bg(panel_area, 0, bg, frame.buffer_mut());
            rendered_areas[idx] = Some(panel_area);
        }

        y_cursor += i32::from(height) + 1;
    }
    rendered_areas
}

fn panel_max_height(viewport_height: u16) -> u16 {
    viewport_height.saturating_sub(2).clamp(6, 12)
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

fn children_for_item<'a>(
    snapshot: &'a RuntimeSnapshot,
    item: &InboxItemRow,
) -> Vec<&'a InboxItemRow> {
    let mut children = snapshot
        .inbox_items
        .iter()
        .filter(|candidate| {
            candidate.parent_id.as_deref() == Some(item.item_id.as_str())
                || candidate.parent_id.as_deref() == Some(item.identifier.as_str())
        })
        .collect::<Vec<_>>();
    children.sort_by(|a, b| a.identifier.cmp(&b.identifier));
    children
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
        Span::styled(tracker::label(snapshot), Style::new().fg(theme::primary())),
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
    display_rows_matching(snapshot, &app.search_query)
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
