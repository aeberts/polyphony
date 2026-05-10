use std::{io, time::Duration};

use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
        MouseEventKind,
    },
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures_util::StreamExt;
use polyphony_core::RuntimeSnapshot;
use polyphony_orchestrator::RuntimeCommand;
use ratatui::{Terminal, backend::CrosstermBackend};
use tokio::sync::{mpsc, watch};

use crate::{
    Error,
    app::{AppState, clamp_selection},
    command_palette,
    render::draw,
    rows::display_rows,
};

pub async fn run(
    mut snapshot_rx: watch::Receiver<RuntimeSnapshot>,
    command_tx: mpsc::UnboundedSender<RuntimeCommand>,
) -> Result<(), Error> {
    enable_raw_mode()?;
    let _cleanup = TerminalCleanup;

    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.hide_cursor()?;

    let _ = command_tx.send(RuntimeCommand::Refresh);

    let mut event_stream = event::EventStream::new();
    let mut app = AppState::default();
    let mut snapshot = snapshot_rx.borrow().clone();
    let mut needs_draw = true;

    loop {
        if needs_draw {
            terminal.draw(|frame| draw(frame, &snapshot, &mut app))?;
            needs_draw = false;
        }

        tokio::select! {
            event = event_stream.next() => {
                match event {
                    Some(Ok(Event::Key(key))) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                        if handle_key(&mut app, key.code, key.modifiers, &snapshot) {
                            break;
                        }
                        needs_draw = true;
                    },
                    Some(Ok(Event::Mouse(mouse))) => {
                        handle_mouse(&mut app, &snapshot, mouse);
                        needs_draw = true;
                    },
                    Some(Ok(Event::Resize(_, _))) => needs_draw = true,
                    Some(Ok(_)) => {},
                    Some(Err(_)) => {},
                    None => break,
                }
            }
            changed = snapshot_rx.changed() => {
                if changed.is_err() {
                    break;
                }
                snapshot = snapshot_rx.borrow().clone();
                clamp_selection(&mut app, display_rows(&snapshot).len());
                needs_draw = true;
            }
            _ = tokio::time::sleep(Duration::from_millis(80)) => {
                app.tick = app.tick.wrapping_add(1);
                if !snapshot.running.is_empty() || snapshot.inbox_items.iter().any(|item| item.status.eq_ignore_ascii_case("in progress")) {
                    needs_draw = true;
                }
            }
        }
    }

    terminal.show_cursor()?;
    Ok(())
}

fn handle_key(
    app: &mut AppState,
    code: KeyCode,
    modifiers: KeyModifiers,
    snapshot: &RuntimeSnapshot,
) -> bool {
    if app.command_palette_open {
        return handle_command_palette_key(app, code, modifiers);
    }

    let rows = display_rows(snapshot);
    match code {
        KeyCode::Esc | KeyCode::Char('q') => return true,
        KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => return true,
        KeyCode::Char('p') if modifiers.contains(KeyModifiers::CONTROL) => {
            app.command_palette_open = true;
            command_palette::reset(app);
        },
        KeyCode::Up | KeyCode::Char('k') => app.selected = app.selected.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => {
            app.selected = (app.selected + 1).min(rows.len().saturating_sub(1));
        },
        KeyCode::PageUp => app.selected = app.selected.saturating_sub(app.visible_rows.max(1)),
        KeyCode::PageDown => {
            app.selected =
                (app.selected + app.visible_rows.max(1)).min(rows.len().saturating_sub(1));
        },
        KeyCode::Home => app.selected = 0,
        KeyCode::End => app.selected = rows.len().saturating_sub(1),
        _ => {},
    }
    false
}

fn handle_command_palette_key(app: &mut AppState, code: KeyCode, modifiers: KeyModifiers) -> bool {
    match code {
        KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => return true,
        KeyCode::Esc => app.command_palette_open = false,
        KeyCode::Enter => app.command_palette_open = false,
        KeyCode::Up | KeyCode::Char('k') => command_palette::selected_up(app),
        KeyCode::Down | KeyCode::Char('j') => command_palette::selected_down(app),
        KeyCode::Char('p') if modifiers.contains(KeyModifiers::CONTROL) => {
            app.command_palette_open = false;
        },
        _ => {},
    }
    false
}

fn handle_mouse(app: &mut AppState, snapshot: &RuntimeSnapshot, mouse: event::MouseEvent) {
    if app.command_palette_open {
        match mouse.kind {
            MouseEventKind::ScrollDown => command_palette::selected_down(app),
            MouseEventKind::ScrollUp => command_palette::selected_up(app),
            _ => {},
        }
        return;
    }

    let rows = display_rows(snapshot);
    match mouse.kind {
        MouseEventKind::ScrollDown => {
            app.selected = (app.selected + 1).min(rows.len().saturating_sub(1));
        },
        MouseEventKind::ScrollUp => app.selected = app.selected.saturating_sub(1),
        MouseEventKind::Up(event::MouseButton::Left) => {
            if let Some(row) = list_row_at_mouse(app, mouse.column, mouse.row) {
                app.selected = row.min(rows.len().saturating_sub(1));
            }
        },
        _ => {},
    }
}

fn list_row_at_mouse(app: &AppState, column: u16, row: u16) -> Option<usize> {
    if !app.list_rect.contains((column, row).into()) || row <= app.list_rect.y {
        return None;
    }
    let visible_row = row.saturating_sub(app.list_rect.y + 1) as usize;
    if visible_row >= app.visible_rows {
        return None;
    }
    Some(app.scroll + visible_row)
}

struct TerminalCleanup;

impl Drop for TerminalCleanup {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = crossterm::execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
    }
}
