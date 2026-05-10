use polyphony_core::{DispatchMode, RuntimeSnapshot};
use ratatui::{
    layout::{Margin, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph, Widget},
};

use crate::{app::AppState, theme};

const MODES: &[(DispatchMode, &str)] = &[
    (
        DispatchMode::Manual,
        "Dispatch only when explicitly requested",
    ),
    (
        DispatchMode::Automatic,
        "Dispatch eligible inbox items automatically",
    ),
    (DispatchMode::Nightshift, "Let Polyphony work unattended"),
    (DispatchMode::Idle, "Keep the orchestrator idle"),
    (DispatchMode::Stop, "Stop global dispatch"),
];

pub(crate) fn open(app: &mut AppState, current: DispatchMode) {
    app.dispatch_mode_picker_open = true;
    app.dispatch_mode_selected = MODES
        .iter()
        .position(|(mode, _)| *mode == current)
        .unwrap_or_default();
}

pub(crate) fn selected_mode(app: &AppState) -> DispatchMode {
    MODES
        .get(app.dispatch_mode_selected)
        .map(|(mode, _)| *mode)
        .unwrap_or(DispatchMode::Manual)
}

pub(crate) fn selected_down(app: &mut AppState) {
    app.dispatch_mode_selected =
        (app.dispatch_mode_selected + 1).min(MODES.len().saturating_sub(1));
}

pub(crate) fn selected_up(app: &mut AppState) {
    app.dispatch_mode_selected = app.dispatch_mode_selected.saturating_sub(1);
}

pub(crate) fn render(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    snapshot: &RuntimeSnapshot,
    app: &AppState,
) {
    let width = 68u16.min(area.width.saturating_sub(4)).max(42);
    let height = 10u16.min(area.height.saturating_sub(4)).max(8);
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

    let inner = modal.inner(Margin::new(2, 1));
    let current = snapshot.dispatch_mode;
    let title_gap = inner
        .width
        .saturating_sub("Dispatch Mode".len() as u16 + "esc".len() as u16);
    let mut lines = vec![Line::from(vec![
        Span::styled("Dispatch Mode", Style::new().fg(theme::text()).bold()),
        Span::raw(" ".repeat(title_gap as usize)),
        Span::styled("esc", Style::new().fg(theme::muted())),
    ])];
    lines.push(Line::raw(""));

    for (idx, (mode, description)) in MODES.iter().enumerate() {
        let selected = idx == app.dispatch_mode_selected;
        let active = *mode == current;
        let bg = if selected {
            theme::bg()
        } else {
            theme::element()
        };
        let marker = if active {
            "• "
        } else {
            "  "
        };
        let mode_text = mode.to_string();
        let prefix = format!("{marker}{mode_text}: ");
        let row_width = inner.width as usize;
        let row_len = prefix.chars().count() + description.chars().count();
        let fill = row_width.saturating_sub(row_len);
        let line = Line::from(vec![
            Span::styled(marker, Style::new().fg(mode_color(*mode)).bg(bg)),
            Span::styled(
                mode_text,
                Style::new()
                    .fg(theme::text())
                    .bg(bg)
                    .add_modifier(if selected {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            ),
            Span::styled(": ", Style::new().fg(theme::muted()).bg(bg)),
            Span::styled(*description, Style::new().fg(theme::muted()).bg(bg)),
            Span::styled(" ".repeat(fill), Style::new().bg(bg)),
        ]);
        lines.push(line);
    }

    Paragraph::new(lines)
        .style(Style::new().bg(theme::element()))
        .render(inner, frame.buffer_mut());
}

fn mode_color(mode: DispatchMode) -> ratatui::style::Color {
    match mode {
        DispatchMode::Stop => theme::error(),
        DispatchMode::Idle | DispatchMode::Manual => theme::secondary(),
        DispatchMode::Automatic | DispatchMode::Nightshift => theme::done(),
    }
}
