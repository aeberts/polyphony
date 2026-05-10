use ratatui::layout::Rect;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Route {
    #[default]
    Inbox,
    Detail,
}

#[derive(Default)]
pub(crate) struct AppState {
    pub route: Route,
    pub selected: usize,
    pub scroll: usize,
    pub detail_scroll: u16,
    pub detail_follow_bottom: bool,
    pub detail_scroll_max: u16,
    pub detail_scrollbar_rect: Rect,
    pub detail_scrollbar_active: bool,
    pub visible_rows: usize,
    pub list_rect: Rect,
    pub search_query: String,
    pub input: String,
    pub tick: u32,
    pub command_palette_open: bool,
    pub command_selected: usize,
    pub command_scroll: usize,
    pub children_expanded: bool,
    pub children_expand_rect: Rect,
    pub mouse_pos: Option<(u16, u16)>,
}

pub(crate) fn clamp_selection(app: &mut AppState, len: usize) {
    app.selected = app.selected.min(len.saturating_sub(1));
}
