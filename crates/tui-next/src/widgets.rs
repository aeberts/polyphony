use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Text},
    widgets::{Block, BorderType, Borders, Padding, Paragraph, Widget, Wrap},
};

use crate::theme;

#[derive(Clone, Copy, Debug)]
pub(crate) struct LeftBorderPanel {
    border_color: Color,
    content_bg: Option<Color>,
    padding: Padding,
}

impl LeftBorderPanel {
    pub(crate) const fn new() -> Self {
        Self {
            border_color: Color::Reset,
            content_bg: None,
            padding: Padding::ZERO,
        }
    }

    #[must_use]
    pub(crate) const fn border_color(mut self, color: Color) -> Self {
        self.border_color = color;
        self
    }

    #[must_use]
    pub(crate) const fn content_bg(mut self, color: Color) -> Self {
        self.content_bg = Some(color);
        self
    }

    #[must_use]
    pub(crate) const fn padding(mut self, padding: Padding) -> Self {
        self.padding = padding;
        self
    }

    pub(crate) fn render(self, area: Rect, buf: &mut Buffer) -> Rect {
        let border_block = Block::new()
            .borders(Borders::LEFT)
            .border_type(BorderType::Thick)
            .border_style(Style::new().fg(self.border_color));

        let after_border = border_block.inner(area);
        border_block.render(area, buf);

        let mut content_block = Block::new().padding(self.padding);
        if let Some(bg) = self.content_bg {
            content_block = content_block.style(Style::new().bg(bg));
        }

        let inner = content_block.inner(after_border);
        content_block.render(after_border, buf);

        inner
    }
}

pub(crate) struct LeftRailPanel {
    lines: Vec<Line<'static>>,
    max_height: Option<u16>,
    bg: Color,
}

impl LeftRailPanel {
    pub(crate) fn new(lines: Vec<Line<'static>>) -> Self {
        Self {
            lines,
            max_height: None,
            bg: theme::element(),
        }
    }

    pub(crate) fn max_height(mut self, max_height: u16) -> Self {
        self.max_height = Some(max_height);
        self
    }

    pub(crate) fn bg(mut self, bg: Color) -> Self {
        self.bg = bg;
        self
    }

    pub(crate) fn height(&self, width: u16) -> u16 {
        let content_width = width.saturating_sub(4).max(1) as usize;
        let content_height = self
            .lines
            .iter()
            .map(|line| line.width().div_ceil(content_width).max(1) as u16)
            .sum::<u16>()
            .max(1);
        content_height + 2
    }

    pub(crate) fn visible_height(&self, width: u16, max_height: u16) -> u16 {
        self.height(width)
            .min(self.max_height.unwrap_or(max_height))
            .max(3)
    }

    pub(crate) fn render_clipped(&self, area: Rect, skip_rows: u16, buf: &mut Buffer) {
        self.render_clipped_with_bg(area, skip_rows, self.bg, buf);
    }

    pub(crate) fn render_clipped_with_bg(
        &self,
        area: Rect,
        skip_rows: u16,
        bg: Color,
        buf: &mut Buffer,
    ) {
        if area.is_empty() {
            return;
        }

        let inner = LeftBorderPanel::new()
            .border_color(theme::border())
            .content_bg(bg)
            .padding(Padding::new(2, 1, 1, 1))
            .render(area, buf);

        if inner.is_empty() {
            return;
        }

        let content_skip = skip_rows.saturating_sub(1) as usize;
        let lines = self
            .lines
            .iter()
            .skip(content_skip)
            .cloned()
            .collect::<Vec<_>>();

        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .render(inner, buf);
    }
}

impl Widget for &LeftRailPanel {
    fn render(self, area: Rect, buf: &mut Buffer) {
        self.render_clipped(area, 0, buf);
    }
}
