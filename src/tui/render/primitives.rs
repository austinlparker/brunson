use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use ratatui::Frame;

use super::component::{Component, RenderContext};
use super::layout::fill;
use super::theme::{BORDER, MANTLE, MUTED, SURFACE0, TEXT};

#[derive(Debug, Clone, Copy)]
pub struct Surface {
    pub bg: Color,
    pub border: Option<Color>,
}

impl Surface {
    pub fn new(bg: Color) -> Self {
        Self { bg, border: None }
    }

    pub fn bordered(bg: Color, border: Color) -> Self {
        Self {
            bg,
            border: Some(border),
        }
    }

    pub fn inner(&self, area: Rect) -> Rect {
        if self.border.is_some() {
            Block::default().borders(Borders::ALL).inner(area)
        } else {
            area
        }
    }

    pub fn render(&self, f: &mut Frame, area: Rect) -> Rect {
        fill(f, area, self.bg);
        if let Some(border) = self.border {
            let block = Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(border).bg(self.bg))
                .style(Style::default().bg(self.bg));
            let inner = block.inner(area);
            f.render_widget(block, area);
            inner
        } else {
            area
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollbarGeometry {
    pub x: u16,
    pub y: u16,
    pub height: u16,
}

pub struct ScrollViewport<'a> {
    pub lines: &'a [Line<'a>],
    pub scroll: usize,
    pub style: Style,
    pub show_scrollbar: bool,
}

impl<'a> ScrollViewport<'a> {
    pub fn new(lines: &'a [Line<'a>], scroll: usize) -> Self {
        Self {
            lines,
            scroll,
            style: Style::default().fg(TEXT).bg(MANTLE),
            show_scrollbar: false,
        }
    }

    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    pub fn scrollbar(mut self, show: bool) -> Self {
        self.show_scrollbar = show;
        self
    }

    pub fn clamped_scroll(&self, height: u16) -> usize {
        clamp_scroll(self.lines.len(), height as usize, self.scroll)
    }

    pub fn visible_range(&self, height: u16) -> std::ops::Range<usize> {
        visible_range(self.lines.len(), height as usize, self.scroll)
    }

    pub fn scrollbar_geometry(&self, area: Rect) -> Option<ScrollbarGeometry> {
        scrollbar_geometry(self.lines.len(), area, self.scroll)
    }

    pub fn render(&self, f: &mut Frame, area: Rect) {
        fill(f, area, self.style.bg.unwrap_or(MANTLE));
        if area.is_empty() {
            return;
        }
        let content_area = if self.show_scrollbar && self.lines.len() > area.height as usize {
            Rect::new(area.x, area.y, area.width.saturating_sub(1), area.height)
        } else {
            area
        };
        let range = self.visible_range(content_area.height);
        let visible: Vec<Line> = self.lines[range].to_vec();
        f.render_widget(Paragraph::new(visible).style(self.style), content_area);

        if self.show_scrollbar {
            if let Some(thumb) = self.scrollbar_geometry(area) {
                for y in area.top()..area.bottom() {
                    if let Some(cell) = f.buffer_mut().cell_mut((thumb.x, y)) {
                        cell.set_symbol("│");
                        cell.set_style(
                            Style::default()
                                .fg(MUTED)
                                .bg(self.style.bg.unwrap_or(MANTLE)),
                        );
                    }
                }
                for y in thumb.y..thumb.y.saturating_add(thumb.height) {
                    if let Some(cell) = f.buffer_mut().cell_mut((thumb.x, y)) {
                        cell.set_symbol("█");
                        cell.set_style(
                            Style::default()
                                .fg(TEXT)
                                .bg(self.style.bg.unwrap_or(MANTLE)),
                        );
                    }
                }
            }
        }
    }
}

pub struct Section<'a> {
    pub title: &'a str,
    pub lines: &'a [Line<'a>],
    pub scroll: usize,
    pub collapsed: bool,
    pub focused: bool,
}

impl<'a> Section<'a> {
    pub fn new(title: &'a str, lines: &'a [Line<'a>]) -> Self {
        Self {
            title,
            lines,
            scroll: 0,
            collapsed: false,
            focused: false,
        }
    }
}

impl Component for Section<'_> {
    fn render(&self, f: &mut Frame, area: Rect, _ctx: &RenderContext) {
        Surface::bordered(MANTLE, if self.focused { TEXT } else { BORDER }).render(f, area);
        if area.width < 2 || area.height < 2 {
            return;
        }
        let title_area = Rect::new(area.x + 1, area.y, area.width.saturating_sub(2), 1);
        let marker = if self.collapsed { "▸" } else { "▾" };
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    format!("{} ", marker),
                    Style::default().fg(MUTED).bg(MANTLE),
                ),
                Span::styled(
                    self.title.to_string(),
                    Style::default()
                        .fg(TEXT)
                        .bg(MANTLE)
                        .add_modifier(Modifier::BOLD),
                ),
            ]))
            .style(Style::default().bg(MANTLE)),
            title_area,
        );
        if !self.collapsed && area.height > 2 {
            let body = Rect::new(
                area.x + 1,
                area.y + 1,
                area.width.saturating_sub(2),
                area.height.saturating_sub(2),
            );
            ScrollViewport::new(self.lines, self.scroll)
                .style(Style::default().fg(TEXT).bg(MANTLE))
                .scrollbar(true)
                .render(f, body);
        }
    }
}

pub struct TextFlow;

impl TextFlow {
    pub fn wrap(text: &str, width: usize) -> Vec<Line<'static>> {
        if width == 0 {
            return Vec::new();
        }
        textwrap::wrap(text, width)
            .into_iter()
            .map(|line| Line::from(line.into_owned()))
            .collect()
    }

    pub fn markdown(text: &str, width: usize) -> Vec<Line<'static>> {
        crate::tui::views::markdown::markdown_lines(text, width)
    }
}

#[derive(Debug, Clone)]
pub struct Divider {
    pub label: Option<String>,
    pub color: Color,
}

impl Default for Divider {
    fn default() -> Self {
        Self::new()
    }
}

impl Divider {
    pub fn new() -> Self {
        Self {
            label: None,
            color: SURFACE0,
        }
    }

    pub fn labeled(label: impl Into<String>) -> Self {
        Self {
            label: Some(label.into()),
            color: SURFACE0,
        }
    }
}

impl Component for Divider {
    fn render(&self, f: &mut Frame, area: Rect, _ctx: &RenderContext) {
        fill(f, area, MANTLE);
        if area.is_empty() {
            return;
        }
        let label = self.label.as_deref().unwrap_or("");
        let text = if label.is_empty() {
            "─".repeat(area.width as usize)
        } else {
            let decorated = format!("─ {} ", label);
            let mut s = decorated;
            s.push_str(&"─".repeat(area.width as usize));
            s.chars().take(area.width as usize).collect()
        };
        f.render_widget(
            Paragraph::new(text).style(Style::default().fg(self.color).bg(MANTLE)),
            area,
        );
    }
}

pub fn clamp_scroll(content_len: usize, viewport_height: usize, requested: usize) -> usize {
    requested.min(content_len.saturating_sub(viewport_height))
}

pub fn visible_range(
    content_len: usize,
    viewport_height: usize,
    requested: usize,
) -> std::ops::Range<usize> {
    let start = clamp_scroll(content_len, viewport_height, requested);
    let end = (start + viewport_height).min(content_len);
    start..end
}

pub fn scrollbar_geometry(
    content_len: usize,
    area: Rect,
    requested: usize,
) -> Option<ScrollbarGeometry> {
    if area.height == 0 || area.width == 0 || content_len <= area.height as usize {
        return None;
    }
    let track = area.height as usize;
    let thumb_h = ((track * track) / content_len).max(1).min(track) as u16;
    let max_scroll = content_len.saturating_sub(track);
    let scroll = requested.min(max_scroll);
    let travel = area.height.saturating_sub(thumb_h) as usize;
    let thumb_y = (if max_scroll == 0 {
        0
    } else {
        scroll
            .checked_mul(travel)
            .and_then(|p| p.checked_add(max_scroll / 2))
            .and_then(|p| p.checked_div(max_scroll))
            .unwrap_or(0)
    }) as u16;
    Some(ScrollbarGeometry {
        x: area.right().saturating_sub(1),
        y: area.y + thumb_y,
        height: thumb_h,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(n: usize) -> Vec<Line<'static>> {
        (0..n).map(|i| Line::from(format!("line {i}"))).collect()
    }

    #[test]
    fn viewport_slices_visible_lines() {
        let lines = lines(10);
        let vp = ScrollViewport::new(&lines, 3);
        assert_eq!(vp.visible_range(4), 3..7);
    }

    #[test]
    fn viewport_clamps_scroll_past_end() {
        let lines = lines(10);
        let vp = ScrollViewport::new(&lines, 99);
        assert_eq!(vp.clamped_scroll(4), 6);
        assert_eq!(vp.visible_range(4), 6..10);
    }

    #[test]
    fn scrollbar_position_tracks_scroll() {
        let area = Rect::new(2, 5, 20, 10);
        let top = scrollbar_geometry(100, area, 0).unwrap();
        let mid = scrollbar_geometry(100, area, 45).unwrap();
        let bottom = scrollbar_geometry(100, area, 90).unwrap();
        assert_eq!(top.x, 21);
        assert_eq!(top.y, 5);
        assert!(mid.y > top.y);
        assert_eq!(bottom.y + bottom.height, area.bottom());
    }
}
