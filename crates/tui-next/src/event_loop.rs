use std::{io, time::Duration};

use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
        MouseEventKind,
    },
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures_util::StreamExt;
use polyphony_core::{DispatchMode, InboxItemKind, RunStatus, RuntimeSnapshot, TaskStatus};
use polyphony_orchestrator::RuntimeCommand;
use ratatui::{Terminal, backend::CrosstermBackend};
use tokio::sync::{mpsc, watch};

use crate::{
    Error,
    app::{AppState, Route, clamp_selection},
    command_palette,
    render::draw,
    rows::display_rows_matching,
};

const DETAIL_MOUSE_SCROLL_ROWS: u16 = 5;

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
                        if handle_key(&mut app, key.code, key.modifiers, &snapshot, &command_tx) {
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
                let row_count = display_rows_matching(&snapshot, &app.search_query).len();
                clamp_selection(&mut app, row_count);
                needs_draw = true;
            }
            _ = tokio::time::sleep(Duration::from_millis(80)) => {
                app.tick = app.tick.wrapping_add(1);
                if app.route == Route::Inbox || !snapshot.running.is_empty() || snapshot.inbox_items.iter().any(|item| item.status.eq_ignore_ascii_case("in progress")) {
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
    command_tx: &mpsc::UnboundedSender<RuntimeCommand>,
) -> bool {
    if app.command_palette_open {
        return handle_command_palette_key(app, code, modifiers, command_tx);
    }

    let rows = display_rows_matching(snapshot, &app.search_query);
    match code {
        KeyCode::Esc
            if app.route == Route::Detail
                && app.detail_input_mode != crate::app::DetailInputMode::None =>
        {
            app.detail_input_mode = crate::app::DetailInputMode::None;
            app.input.clear();
        },
        KeyCode::Esc if app.route == Route::Inbox && !app.search_query.is_empty() => {
            app.search_query.clear();
            app.selected = 0;
            app.scroll = 0;
        },
        KeyCode::Esc if app.route == Route::Detail => {
            app.route = Route::Inbox;
            app.detail_scroll = 0;
            app.detail_follow_bottom = false;
            app.children_expanded = false;
            app.detail_input_mode = crate::app::DetailInputMode::None;
            app.input.clear();
        },
        KeyCode::Esc => return true,
        KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => return true,
        KeyCode::Char('p') if modifiers.contains(KeyModifiers::CONTROL) => {
            app.command_palette_open = true;
            command_palette::reset(app);
        },
        KeyCode::Enter if app.route == Route::Inbox && !rows.is_empty() => {
            app.route = Route::Detail;
            app.detail_follow_bottom = true;
            app.children_expanded = false;
        },
        KeyCode::Enter
            if app.route == Route::Detail
                && app.detail_input_mode == crate::app::DetailInputMode::Hijack =>
        {
            submit_hijack(app, snapshot, command_tx);
        },
        KeyCode::Enter | KeyCode::Char('d')
            if app.route == Route::Detail
                && app.detail_input_mode == crate::app::DetailInputMode::None =>
        {
            dispatch_selected(app, snapshot, command_tx);
        },
        KeyCode::Char('s')
            if app.route == Route::Detail
                && app.detail_input_mode == crate::app::DetailInputMode::None =>
        {
            stop_selected(app, snapshot, command_tx);
        },
        KeyCode::Char('p')
            if app.route == Route::Detail
                && app.detail_input_mode == crate::app::DetailInputMode::None =>
        {
            pause_or_resume_selected(app, snapshot, command_tx);
        },
        KeyCode::Char('r')
            if app.route == Route::Detail
                && app.detail_input_mode == crate::app::DetailInputMode::None =>
        {
            retry_selected(app, snapshot, command_tx);
        },
        KeyCode::Char('h')
            if app.route == Route::Detail
                && app.detail_input_mode == crate::app::DetailInputMode::None =>
        {
            app.detail_input_mode = crate::app::DetailInputMode::Hijack;
            app.input.clear();
        },
        KeyCode::Up if app.route == Route::Detail => {
            app.detail_scroll = app.detail_scroll.saturating_sub(1);
            app.detail_follow_bottom = false;
        },
        KeyCode::Down if app.route == Route::Detail => {
            app.detail_scroll = app.detail_scroll.saturating_add(1);
            app.detail_follow_bottom = false;
        },
        KeyCode::PageUp if app.route == Route::Detail => {
            app.detail_scroll = app.detail_scroll.saturating_sub(8);
            app.detail_follow_bottom = false;
        },
        KeyCode::PageDown if app.route == Route::Detail => {
            app.detail_scroll = app.detail_scroll.saturating_add(8);
            app.detail_follow_bottom = false;
        },
        KeyCode::Backspace
            if app.route == Route::Detail
                && app.detail_input_mode == crate::app::DetailInputMode::Hijack =>
        {
            app.input.pop();
        },
        KeyCode::Char(c)
            if app.route == Route::Detail
                && app.detail_input_mode == crate::app::DetailInputMode::Hijack
                && is_search_char(c, modifiers) =>
        {
            app.input.push(c);
        },
        KeyCode::Backspace if app.route == Route::Inbox => {
            app.search_query.pop();
            app.selected = app.selected.min(rows.len().saturating_sub(1));
        },
        KeyCode::Char(c) if app.route == Route::Inbox && is_search_char(c, modifiers) => {
            app.search_query.push(c);
            app.selected = 0;
            app.scroll = 0;
        },
        KeyCode::Up => app.selected = app.selected.saturating_sub(1),
        KeyCode::Down => {
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

fn is_search_char(c: char, modifiers: KeyModifiers) -> bool {
    if modifiers.contains(KeyModifiers::CONTROL)
        || modifiers.contains(KeyModifiers::ALT)
        || modifiers.contains(KeyModifiers::SUPER)
    {
        return false;
    }

    c.is_ascii() && !c.is_ascii_control()
}

fn selected_item(
    snapshot: &RuntimeSnapshot,
    app: &AppState,
) -> Option<polyphony_core::InboxItemRow> {
    display_rows_matching(snapshot, &app.search_query)
        .get(app.selected)
        .and_then(|row| snapshot.inbox_items.get(row.item_idx))
        .cloned()
}

fn dispatch_selected(
    app: &mut AppState,
    snapshot: &RuntimeSnapshot,
    command_tx: &mpsc::UnboundedSender<RuntimeCommand>,
) {
    let Some(item) = selected_item(snapshot, app) else {
        app.status_message = Some("no inbox item selected".to_string());
        return;
    };
    dispatch_item(&item, None, command_tx);
    app.detail_follow_bottom = true;
    record_notice(
        app,
        &item,
        format!("dispatch queued for {}", item.identifier),
    );
}

fn dispatch_item(
    item: &polyphony_core::InboxItemRow,
    directives: Option<String>,
    command_tx: &mpsc::UnboundedSender<RuntimeCommand>,
) {
    match item.kind {
        InboxItemKind::Issue => {
            let _ = command_tx.send(RuntimeCommand::DispatchIssue {
                issue_id: item.item_id.clone(),
                agent_name: None,
                directives,
            });
        },
        InboxItemKind::PullRequestReview
        | InboxItemKind::PullRequestComment
        | InboxItemKind::PullRequestConflict => {
            let _ = command_tx.send(RuntimeCommand::DispatchPullRequestInboxItem {
                item_id: item.item_id.clone(),
                directives,
            });
        },
    }
}

fn stop_selected(
    app: &mut AppState,
    snapshot: &RuntimeSnapshot,
    command_tx: &mpsc::UnboundedSender<RuntimeCommand>,
) {
    let Some(item) = selected_item(snapshot, app) else {
        app.status_message = Some("no inbox item selected".to_string());
        return;
    };
    let _ = command_tx.send(RuntimeCommand::StopAgent {
        issue_id: item.item_id.clone(),
    });
    app.detail_follow_bottom = true;
    record_notice(
        app,
        &item,
        format!("stop requested for {}", item.identifier),
    );
}

fn pause_or_resume_selected(
    app: &mut AppState,
    snapshot: &RuntimeSnapshot,
    command_tx: &mpsc::UnboundedSender<RuntimeCommand>,
) {
    let Some(item) = selected_item(snapshot, app) else {
        app.status_message = Some("no inbox item selected".to_string());
        return;
    };
    if issue_has_running_agent(snapshot, &item) {
        let _ = command_tx.send(RuntimeCommand::StopAgent {
            issue_id: item.item_id.clone(),
        });
        record_notice(
            app,
            &item,
            format!("pause requested for {}", item.identifier),
        );
        app.detail_follow_bottom = true;
        return;
    }
    if retry_latest_stopped(snapshot, &item, command_tx) {
        record_notice(app, &item, format!("resume queued for {}", item.identifier));
        app.detail_follow_bottom = true;
        return;
    }
    dispatch_item(&item, None, command_tx);
    record_notice(
        app,
        &item,
        format!("dispatch queued for {}", item.identifier),
    );
    app.detail_follow_bottom = true;
}

fn retry_selected(
    app: &mut AppState,
    snapshot: &RuntimeSnapshot,
    command_tx: &mpsc::UnboundedSender<RuntimeCommand>,
) {
    let Some(item) = selected_item(snapshot, app) else {
        app.status_message = Some("no inbox item selected".to_string());
        return;
    };
    if retry_latest_stopped(snapshot, &item, command_tx) {
        record_notice(app, &item, format!("retry queued for {}", item.identifier));
        app.detail_follow_bottom = true;
    } else {
        record_notice(
            app,
            &item,
            "nothing failed or cancelled to retry".to_string(),
        );
    }
}

fn submit_hijack(
    app: &mut AppState,
    snapshot: &RuntimeSnapshot,
    command_tx: &mpsc::UnboundedSender<RuntimeCommand>,
) {
    let prompt = app.input.trim().to_string();
    if prompt.is_empty() {
        app.detail_input_mode = crate::app::DetailInputMode::None;
        app.input.clear();
        if let Some(item) = selected_item(snapshot, app) {
            record_notice(
                app,
                &item,
                "hijack cancelled: empty intervention".to_string(),
            );
        }
        return;
    }
    let Some(item) = selected_item(snapshot, app) else {
        app.status_message = Some("no inbox item selected".to_string());
        return;
    };

    if issue_has_running_agent(snapshot, &item) {
        let _ = command_tx.send(RuntimeCommand::StopAgent {
            issue_id: item.item_id.clone(),
        });
    }
    let run_id = latest_run(snapshot, &item).map(|run| run.id.clone());
    if let Some(run_id) = run_id.clone() {
        let _ = command_tx.send(RuntimeCommand::InjectRunFeedback {
            run_id,
            prompt: prompt.clone(),
            agent_name: None,
        });
    } else {
        dispatch_item(&item, Some(prompt.clone()), command_tx);
    }
    app.interventions.push(crate::app::IssueIntervention {
        issue_id: item.item_id.clone(),
        run_id,
        prompt,
    });
    app.detail_input_mode = crate::app::DetailInputMode::None;
    app.input.clear();
    app.detail_follow_bottom = true;
    record_notice(
        app,
        &item,
        format!("intervention queued for {}", item.identifier),
    );
}

fn record_notice(app: &mut AppState, item: &polyphony_core::InboxItemRow, message: String) {
    app.status_message = None;
    app.notices.push(crate::app::IssueNotice {
        issue_id: item.item_id.clone(),
        message,
    });
    if app.notices.len() > 64 {
        app.notices.remove(0);
    }
}

fn issue_has_running_agent(
    snapshot: &RuntimeSnapshot,
    item: &polyphony_core::InboxItemRow,
) -> bool {
    snapshot
        .running
        .iter()
        .any(|agent| agent.issue_id == item.item_id || agent.issue_identifier == item.identifier)
}

fn retry_latest_stopped(
    snapshot: &RuntimeSnapshot,
    item: &polyphony_core::InboxItemRow,
    command_tx: &mpsc::UnboundedSender<RuntimeCommand>,
) -> bool {
    if let Some(task) = latest_failed_task(snapshot, item) {
        let _ = command_tx.send(RuntimeCommand::RetryTask {
            run_id: task.run_id.clone(),
            task_id: task.id.clone(),
        });
        return true;
    }
    if let Some(run) = latest_run(snapshot, item)
        .filter(|run| matches!(run.status, RunStatus::Failed | RunStatus::Cancelled))
    {
        let _ = command_tx.send(RuntimeCommand::RetryRun {
            run_id: run.id.clone(),
        });
        return true;
    }
    false
}

fn latest_failed_task<'a>(
    snapshot: &'a RuntimeSnapshot,
    item: &polyphony_core::InboxItemRow,
) -> Option<&'a polyphony_core::TaskRow> {
    let run_ids = snapshot
        .runs
        .iter()
        .filter(|run| run.issue_identifier.as_deref() == Some(item.identifier.as_str()))
        .map(|run| &run.id)
        .collect::<Vec<_>>();
    snapshot
        .tasks
        .iter()
        .filter(|task| {
            run_ids.contains(&&task.run_id)
                && matches!(task.status, TaskStatus::Failed | TaskStatus::Cancelled)
        })
        .max_by_key(|task| task.updated_at)
}

fn latest_run<'a>(
    snapshot: &'a RuntimeSnapshot,
    item: &polyphony_core::InboxItemRow,
) -> Option<&'a polyphony_core::RunRow> {
    snapshot
        .runs
        .iter()
        .filter(|run| run.issue_identifier.as_deref() == Some(item.identifier.as_str()))
        .max_by_key(|run| run.created_at)
}

fn handle_command_palette_key(
    app: &mut AppState,
    code: KeyCode,
    modifiers: KeyModifiers,
    command_tx: &mpsc::UnboundedSender<RuntimeCommand>,
) -> bool {
    match code {
        KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => return true,
        KeyCode::Esc => app.command_palette_open = false,
        KeyCode::Enter => run_command_palette_selection(app, command_tx),
        KeyCode::Up | KeyCode::Char('k') => command_palette::selected_up(app),
        KeyCode::Down | KeyCode::Char('j') => command_palette::selected_down(app),
        KeyCode::Char('p') if modifiers.contains(KeyModifiers::CONTROL) => {
            app.command_palette_open = false;
        },
        _ => {},
    }
    false
}

fn run_command_palette_selection(
    app: &mut AppState,
    command_tx: &mpsc::UnboundedSender<RuntimeCommand>,
) {
    match command_palette::COMMANDS
        .get(app.command_selected)
        .map(|command| command.name)
    {
        Some("Refresh trackers") => {
            let _ = command_tx.send(RuntimeCommand::Refresh);
            app.status_message = Some("refresh requested".to_string());
        },
        Some("Stop dispatching") => {
            let _ = command_tx.send(RuntimeCommand::SetMode(DispatchMode::Stop));
            app.status_message = Some("global dispatch stopped".to_string());
        },
        Some("Manual dispatch mode") => {
            let _ = command_tx.send(RuntimeCommand::SetMode(DispatchMode::Manual));
            app.status_message = Some("global dispatch set to manual".to_string());
        },
        Some("Switch to nightshift mode") => {
            let _ = command_tx.send(RuntimeCommand::SetMode(DispatchMode::Nightshift));
            app.status_message = Some("global dispatch set to nightshift".to_string());
        },
        _ => {},
    }
    app.command_palette_open = false;
}

fn handle_mouse(app: &mut AppState, snapshot: &RuntimeSnapshot, mouse: event::MouseEvent) {
    app.mouse_pos = Some((mouse.column, mouse.row));

    if app.command_palette_open {
        match mouse.kind {
            MouseEventKind::ScrollDown => command_palette::selected_down(app),
            MouseEventKind::ScrollUp => command_palette::selected_up(app),
            _ => {},
        }
        return;
    }

    let rows = display_rows_matching(snapshot, &app.search_query);
    match mouse.kind {
        MouseEventKind::ScrollDown => {
            if app.route == Route::Detail {
                app.detail_scroll = app.detail_scroll.saturating_add(DETAIL_MOUSE_SCROLL_ROWS);
                app.detail_follow_bottom = false;
            } else {
                app.selected = (app.selected + 1).min(rows.len().saturating_sub(1));
            }
        },
        MouseEventKind::ScrollUp => {
            if app.route == Route::Detail {
                app.detail_scroll = app.detail_scroll.saturating_sub(DETAIL_MOUSE_SCROLL_ROWS);
                app.detail_follow_bottom = false;
            } else {
                app.selected = app.selected.saturating_sub(1);
            }
        },
        MouseEventKind::Down(event::MouseButton::Left)
        | MouseEventKind::Drag(event::MouseButton::Left)
            if app.route == Route::Detail
                && (app.detail_scrollbar_active
                    || app
                        .detail_scrollbar_rect
                        .contains((mouse.column, mouse.row).into())) =>
        {
            app.detail_scrollbar_active = true;
            app.detail_scroll = scroll_from_scrollbar(app, mouse.row);
            app.detail_follow_bottom = false;
        },
        MouseEventKind::Up(event::MouseButton::Left) => {
            if app.route == Route::Detail && app.detail_scrollbar_active {
                app.detail_scroll = scroll_from_scrollbar(app, mouse.row);
                app.detail_scrollbar_active = false;
                app.detail_follow_bottom = false;
                return;
            }
            if app.route == Route::Detail
                && app
                    .children_expand_rect
                    .contains((mouse.column, mouse.row).into())
            {
                app.children_expanded = !app.children_expanded;
                return;
            }
            if app.route == Route::Inbox
                && let Some(row) = list_row_at_mouse(app, mouse.column, mouse.row)
            {
                app.selected = row.min(rows.len().saturating_sub(1));
                app.route = Route::Detail;
                app.detail_follow_bottom = true;
                app.children_expanded = false;
            }
        },
        _ => {},
    }
}

fn scroll_from_scrollbar(app: &AppState, row: u16) -> u16 {
    if app.detail_scrollbar_rect.is_empty() || app.detail_scroll_max == 0 {
        return app.detail_scroll;
    }

    let top = app.detail_scrollbar_rect.y;
    let track_rows = app.detail_scrollbar_rect.height.saturating_sub(1).max(1);
    let row_offset = row.saturating_sub(top).min(track_rows);
    ((u32::from(row_offset) * u32::from(app.detail_scroll_max)) / u32::from(track_rows)) as u16
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
