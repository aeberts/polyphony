use ratatui::layout::Rect;

#[derive(Default)]
pub(crate) struct AppState {
    pub selected: usize,
    pub scroll: usize,
    pub visible_rows: usize,
    pub list_rect: Rect,
    pub tick: u32,
    pub command_palette_open: bool,
    pub command_selected: usize,
    pub command_scroll: usize,
}

pub(crate) fn clamp_selection(app: &mut AppState, len: usize) {
    app.selected = app.selected.min(len.saturating_sub(1));
}
