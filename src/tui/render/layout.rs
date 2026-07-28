use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};
use ratatui::Frame;

pub use super::theme::Blade;
use super::theme::{BASE, INBOX, MANTLE, SURFACE1, TEXT};

/// Geometry for a single blade within the body area. With only one blade
/// visible at a time (the tab line shows the rest), every blade's content is
/// the full body; `is_active` marks which one is currently drawn.
#[derive(Debug, Clone, Copy)]
pub struct BladeLayout {
    pub rect: Rect,
    pub blade: Blade,
    pub is_active: bool,
    pub content: Rect,
}

/// Complete layout produced by `RootLayout` for one frame.
#[derive(Debug, Clone, Copy)]
pub struct ViewLayout {
    pub terminal: Rect,
    pub tab_line: Rect,
    pub body: Rect,
    pub command_line: Rect,
    pub keybar: Rect,
    pub blades: [BladeLayout; 5],
}

impl ViewLayout {
    pub fn active_content(&self) -> Rect {
        self.blades
            .iter()
            .find(|b| b.is_active)
            .map(|b| b.content)
            .unwrap_or(self.body)
    }

    pub fn blade(&self, blade: Blade) -> &BladeLayout {
        &self.blades[blade.index()]
    }
}

/// Owns the outer vertical geometry of the TUI: a tab line, the full-bleed
/// body of the active blade, and the status/keybar chrome, separated by rules.
#[derive(Debug, Clone, Copy, Default)]
pub struct RootLayout {
    pub active: Blade,
}

impl RootLayout {
    pub const MIN_WIDTH: u16 = 50;
    pub const MIN_HEIGHT: u16 = 12;

    pub fn new(active: Blade) -> Self {
        Self { active }
    }

    pub fn compute(&self, area: Rect) -> ViewLayout {
        // tab line · rule · body · rule · status · keybar
        let vertical = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Fill(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(area);

        let tab_line = vertical[0];
        let body = vertical[2];
        let command_line = vertical[4];
        let keybar = vertical[5];
        // Only one blade is drawn at a time and it occupies the whole body, so
        // every blade's content area is the body; `prepare` clamps scroll using
        // these dimensions regardless of which blade is currently active.
        let blades = std::array::from_fn(|i| {
            let blade = Blade::from_index(i);
            BladeLayout {
                rect: body,
                blade,
                is_active: blade == self.active,
                content: body,
            }
        });

        ViewLayout {
            terminal: area,
            tab_line,
            body,
            command_line,
            keybar,
            blades,
        }
    }

    /// Paint the frame background and the two horizontal rules that bracket the
    /// body. Every allocated cell is painted with an explicit background; the
    /// tab line, body, and bottom chrome are filled by their own renderers.
    pub fn render(&self, f: &mut Frame, area: Rect) -> ViewLayout {
        let layout = self.compute(area);
        fill(f, area, BASE);
        // The rules occupy the single rows directly under the tab line and under
        // the body, bracketing the full-bleed active blade.
        render_rule(
            f,
            Rect::new(area.x, layout.tab_line.bottom(), area.width, 1),
        );
        render_rule(f, Rect::new(area.x, layout.body.bottom(), area.width, 1));
        layout
    }

    pub fn is_sufficient(&self, area: Rect) -> bool {
        area.width >= Self::MIN_WIDTH && area.height >= Self::MIN_HEIGHT
    }

    pub fn render_splash(&self, f: &mut Frame, area: Rect) {
        fill(f, area, BASE);
        let popup = minimum_size_splash_rect(area);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(INBOX))
            .style(Style::default().fg(TEXT).bg(MANTLE));
        let inner = block.inner(popup);
        f.render_widget(block, popup);
        f.render_widget(
            Paragraph::new("Terminal too small\nNeed at least 50×12")
                .style(Style::default().fg(TEXT).bg(MANTLE))
                .alignment(Alignment::Center)
                .wrap(Wrap { trim: false }),
            inner,
        );
    }
}

/// Draw a single-row horizontal rule across `area`.
fn render_rule(f: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let rule = "─".repeat(area.width as usize);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            rule,
            Style::default().fg(SURFACE1).bg(BASE),
        ))),
        area,
    );
}

pub(crate) fn fill(f: &mut Frame, area: Rect, bg: Color) {
    let style = Style::default().bg(bg);
    let buf = f.buffer_mut();
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_symbol(" ");
                cell.set_style(style);
            }
        }
    }
}

fn minimum_size_splash_rect(area: Rect) -> Rect {
    centered_rect(area, 28, 4)
}

fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    let x = area.x + area.width.saturating_sub(w) / 2;
    let y = area.y + area.height.saturating_sub(h) / 2;
    Rect::new(x, y, w, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_blade_gets_full_body_width() {
        let area = Rect::new(0, 0, 100, 40);
        for i in 0..Blade::count() {
            let layout = RootLayout::new(Blade::from_index(i)).compute(area);
            for (j, blade) in layout.blades.iter().enumerate() {
                assert_eq!(blade.rect, layout.body);
                assert_eq!(blade.content, layout.body);
                assert_eq!(blade.rect.width, area.width);
                assert_eq!(blade.is_active, i == j);
            }
        }
    }

    #[test]
    fn chrome_rows_bracket_a_full_bleed_body() {
        let layout = RootLayout::new(Blade::Inbox).compute(Rect::new(0, 0, 80, 24));
        // tab line + rule + body + rule + status + keybar = 5 chrome rows.
        assert_eq!(layout.tab_line.height, 1);
        assert_eq!(layout.command_line.height, 1);
        assert_eq!(layout.keybar.height, 1);
        assert_eq!(layout.body.height, 19);
        assert_eq!(layout.body.width, 80);
        // The tab line sits above the body, which sits above the chrome.
        assert!(layout.tab_line.bottom() <= layout.body.top());
        assert!(layout.body.bottom() <= layout.command_line.top());
        assert!(layout.command_line.bottom() <= layout.keybar.top());
    }

    #[test]
    fn minimum_size_splash_rect_fits_and_centers() {
        let area = Rect::new(0, 0, 40, 10);
        let popup = minimum_size_splash_rect(area);
        assert!(popup.width <= area.width);
        assert!(popup.height <= area.height);
        assert_eq!(popup.x, 6);
        assert_eq!(popup.y, 3);
    }
}
