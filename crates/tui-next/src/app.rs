use ratatui::layout::Rect;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Route {
    #[default]
    Inbox,
    Detail,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum DetailInputMode {
    #[default]
    None,
    Hijack,
}

#[derive(Clone, Debug)]
pub(crate) struct IssueIntervention {
    pub issue_id: String,
    pub run_id: Option<String>,
    pub prompt: String,
}

#[derive(Clone, Debug)]
pub(crate) struct IssueNotice {
    pub issue_id: String,
    pub message: String,
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
    pub detail_input_mode: DetailInputMode,
    pub status_message: Option<String>,
    pub interventions: Vec<IssueIntervention>,
    pub notices: Vec<IssueNotice>,
    pub tick: u32,
    pub command_palette_open: bool,
    pub command_query: String,
    pub command_selected: usize,
    pub command_scroll: usize,
    pub dispatch_mode_picker_open: bool,
    pub dispatch_mode_selected: usize,
    pub children_expanded: bool,
    pub children_expand_rect: Rect,
    pub workspace_path_rect: Rect,
    pub workspace_path_to_copy: Option<String>,
    pub toast_message: Option<String>,
    pub toast_until_tick: u32,
    pub session_text_rect: Rect,
    pub session_visible_lines: Vec<String>,
    pub session_selection_start: Option<(u16, u16)>,
    pub session_selection_end: Option<(u16, u16)>,
    pub session_selecting: bool,
    pub sidebar_text_rect: Rect,
    pub sidebar_visible_lines: Vec<String>,
    pub sidebar_selection_start: Option<(u16, u16)>,
    pub sidebar_selection_end: Option<(u16, u16)>,
    pub sidebar_selecting: bool,
    pub mouse_pos: Option<(u16, u16)>,
}

pub(crate) fn clamp_selection(app: &mut AppState, len: usize) {
    app.selected = app.selected.min(len.saturating_sub(1));
}
