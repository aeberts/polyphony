use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Widget},
};

use crate::{app::AppState, theme};

pub(crate) struct Command {
    pub section: &'static str,
    pub name: &'static str,
    pub description: &'static str,
}

pub(crate) const COMMANDS: &[Command] = &[
    Command {
        section: "Inbox",
        name: "Open selected inbox item",
        description: "Focus the currently selected orchestration",
    },
    Command {
        section: "Inbox",
        name: "Create new beads issue",
        description: "Draft a local issue for this repo",
    },
    Command {
        section: "Inbox",
        name: "Sort by oldest",
        description: "Show the oldest inbox item first",
    },
    Command {
        section: "Inbox",
        name: "Sort by priority",
        description: "Group urgent work at the top",
    },
    Command {
        section: "Runtime",
        name: "Refresh trackers",
        description: "Fetch GitHub, GitLab, Linear, and beads updates",
    },
    Command {
        section: "Runtime",
        name: "Stop dispatching",
        description: "Pause automatic agent dispatch",
    },
    Command {
        section: "Runtime",
        name: "Manual dispatch mode",
        description: "Resume issue-scoped manual dispatch",
    },
    Command {
        section: "Runtime",
        name: "Switch to nightshift mode",
        description: "Let Polyphony work unattended",
    },
    Command {
        section: "Agents",
        name: "Dispatch implementer",
        description: "Assign implementation work to an agent",
    },
    Command {
        section: "Agents",
        name: "Dispatch reviewer",
        description: "Ask an agent to review the selected work",
    },
    Command {
        section: "View",
        name: "Toggle logs overlay",
        description: "Inspect runtime events and agent logs",
    },
    Command {
        section: "View",
        name: "Open repository settings",
        description: "Manage tracker and repo configuration",
    },
];

pub(crate) fn selected_down(app: &mut AppState) {
    app.command_selected = (app.command_selected + 1).min(COMMANDS.len().saturating_sub(1));
}

pub(crate) fn selected_up(app: &mut AppState) {
    app.command_selected = app.command_selected.saturating_sub(1);
}

pub(crate) fn reset(app: &mut AppState) {
    app.command_selected = 0;
    app.command_scroll = 0;
}

pub(crate) fn render(frame: &mut ratatui::Frame<'_>, area: Rect, app: &mut AppState) {
    let width = 74u16.min(area.width.saturating_sub(4)).max(42);
    let height = 18u16.min(area.height.saturating_sub(4)).max(8);
    let modal = Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, modal);
    frame.render_widget(
        Block::default().style(Style::new().bg(theme::element())),
        modal,
    );

    let inner = modal.inner(ratatui::layout::Margin::new(2, 1));
    let content = Rect::new(
        inner.x,
        inner.y,
        inner.width.saturating_sub(2),
        inner.height,
    );
    let visible_commands = visible_command_count(content.height);
    sync_scroll_for_height(app, content.height as usize);
    let mut lines = vec![Line::from(vec![
        Span::styled("Command Palette  ", Style::new().fg(theme::text()).bold()),
        Span::styled("global actions  ", Style::new().fg(theme::muted())),
        Span::styled("Ctrl+P", Style::new().fg(theme::text())),
        Span::styled(":commands  ", Style::new().fg(theme::muted())),
        Span::styled("Enter", Style::new().fg(theme::text())),
        Span::styled(":run  ", Style::new().fg(theme::muted())),
        Span::styled("Esc", Style::new().fg(theme::text())),
        Span::styled(":close", Style::new().fg(theme::muted())),
    ])];
    lines.push(Line::raw(""));

    let mut last_section = "";
    for (idx, command) in COMMANDS
        .iter()
        .enumerate()
        .skip(app.command_scroll)
        .take(visible_commands)
    {
        if command.section != last_section {
            lines.push(Line::styled(
                command.section,
                Style::new().fg(theme::muted()),
            ));
            last_section = command.section;
        }
        let selected = idx == app.command_selected;
        let bg = if selected {
            theme::bg()
        } else {
            theme::element()
        };
        let marker = if selected {
            "▶ "
        } else {
            "  "
        };
        lines.push(Line::from(vec![
            Span::styled(marker, Style::new().fg(theme::primary()).bg(bg)),
            Span::styled(
                command.name,
                Style::new()
                    .fg(theme::text())
                    .bg(bg)
                    .add_modifier(if selected {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::styled("  ", Style::new().bg(bg)),
            Span::styled(command.description, Style::new().fg(theme::muted()).bg(bg)),
        ]));
    }

    Paragraph::new(lines)
        .style(Style::new().bg(theme::element()))
        .render(content, frame.buffer_mut());

    if COMMANDS.len() > visible_commands {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(Some(" "))
            .track_style(Style::new().bg(theme::bg()))
            .thumb_symbol(" ")
            .thumb_style(Style::new().bg(theme::border()));
        let scrollbar_area = Rect::new(
            inner.x + inner.width.saturating_sub(1),
            inner.y + 2,
            1,
            inner.height.saturating_sub(2),
        );
        let max_scroll = COMMANDS.len().saturating_sub(visible_commands);
        let track_height = scrollbar_area.height;
        let thumb_height = ((u32::from(track_height) * visible_commands as u32)
            / COMMANDS.len() as u32)
            .max(1)
            .min(u32::from(track_height.saturating_sub(1)));
        let scroll_scale = u32::from(track_height).saturating_sub(thumb_height);
        let content_length = max_scroll.saturating_mul(scroll_scale as usize) + 1;
        let viewport_length = thumb_height.saturating_mul(max_scroll as u32).max(1);
        let mut state = ScrollbarState::new(content_length)
            .position(app.command_scroll.saturating_mul(scroll_scale as usize))
            .viewport_content_length(viewport_length as usize);
        frame.render_stateful_widget(scrollbar, scrollbar_area, &mut state);
    }
}

fn visible_command_count(content_height: u16) -> usize {
    // Two header lines plus roughly two lines per command. Section labels are
    // extra, so keep this conservative to make overflow visible and stable.
    (content_height.saturating_sub(2) / 2).max(1) as usize
}

fn sync_scroll_for_height(app: &mut AppState, content_height: usize) {
    let max_scroll = COMMANDS.len().saturating_sub(1);
    app.command_scroll = app.command_scroll.min(max_scroll);
    if app.command_selected < app.command_scroll {
        app.command_scroll = app.command_selected;
    }

    while rendered_lines_through_selected(app.command_scroll, app.command_selected)
        > content_height.max(1)
        && app.command_scroll < app.command_selected
    {
        app.command_scroll += 1;
    }
}

fn rendered_lines_through_selected(scroll: usize, selected: usize) -> usize {
    if selected < scroll {
        return 0;
    }

    let mut lines = 2; // header plus spacer
    let mut last_section = "";
    for command in COMMANDS.iter().take(selected + 1).skip(scroll) {
        if command.section != last_section {
            lines += 1;
            last_section = command.section;
        }
        lines += 2;
    }
    lines
}
