use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};
use ratatui::Frame;

pub use super::theme::Blade;
use super::theme::{BASE, INBOX, MANTLE, TEXT};

/// Geometry for a single blade within the body area.
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

/// Owns the outer vertical/horizontal geometry of the TUI and renders the
/// surrounding chrome and collapsed blade strips.
#[derive(Debug, Clone, Copy, Default)]
pub struct RootLayout {
    pub active: Blade,
}

impl RootLayout {
    pub const MIN_WIDTH: u16 = 50;
    pub const MIN_HEIGHT: u16 = 12;
    pub const CHROME_ROWS: u16 = 2;
    pub const COLLAPSED_WIDTH: u16 = 4;

    pub fn new(active: Blade) -> Self {
        Self { active }
    }

    pub fn compute(&self, area: Rect) -> ViewLayout {
        let vertical = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Fill(1),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(area);

        let body = vertical[0];
        let command_line = vertical[1];
        let keybar = vertical[2];
        let blade_rects = layout_blades(body, self.active);
        let blades = std::array::from_fn(|i| {
            let blade = Blade::from_index(i);
            let rect = blade_rects[i];
            let is_active = blade == self.active;
            let content = if is_active {
                active_blade_inner(rect)
            } else {
                collapsed_blade_inner(rect)
            };
            BladeLayout {
                rect,
                blade,
                is_active,
                content,
            }
        });

        ViewLayout {
            terminal: area,
            body,
            command_line,
            keybar,
            blades,
        }
    }

    /// Render collapsed strips and the active blade border. Every allocated cell
    /// is painted with an explicit background.
    pub fn render(&self, f: &mut Frame, area: Rect) -> ViewLayout {
        let layout = self.compute(area);
        fill(f, area, BASE);
        for blade_layout in &layout.blades {
            if blade_layout.is_active {
                render_active_blade_border(f, blade_layout.rect, blade_layout.blade);
            } else {
                render_collapsed_strip(f, blade_layout.rect, blade_layout.blade, self.active);
            }
        }
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

pub fn layout_blades(area: Rect, active: Blade) -> [Rect; 5] {
    let constraints: [Constraint; 5] = std::array::from_fn(|i| {
        if i == active.index() {
            Constraint::Fill(1)
        } else {
            Constraint::Length(RootLayout::COLLAPSED_WIDTH)
        }
    });
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(constraints)
        .split(area);
    std::array::from_fn(|i| chunks[i])
}

fn active_blade_inner(rect: Rect) -> Rect {
    Block::default().borders(Borders::ALL).inner(rect)
}

fn collapsed_blade_inner(rect: Rect) -> Rect {
    let mut r = rect;
    r.x = r.x.saturating_add(1);
    r.width = r.width.saturating_sub(1);
    r
}

fn render_collapsed_strip(f: &mut Frame, area: Rect, blade: Blade, _active: Blade) {
    let accent = blade.accent();
    fill(f, area, MANTLE);
    let block = Block::default()
        .borders(Borders::LEFT)
        .border_style(Style::default().fg(accent))
        .style(Style::default().bg(MANTLE));
    f.render_widget(block, area);

    let inner = collapsed_blade_inner(area);
    if inner.width == 0 || inner.height < 3 {
        return;
    }

    // Center the single-width blade icon vertically and horizontally in the
    // collapsed strip. The inner width is 3 cells, so the glyph sits between one
    // blank cell on each side.
    let top_pad = inner.height.saturating_sub(1) / 2;
    let left_pad = inner.width.saturating_sub(1) / 2;
    let right_pad = inner.width.saturating_sub(1).saturating_sub(left_pad);
    let mut lines = Vec::new();
    for _ in 0..top_pad {
        lines.push(Line::from(""));
    }
    lines.push(Line::from(vec![Span::styled(
        format!(
            "{}{}{}",
            " ".repeat(left_pad as usize),
            blade.icon(),
            " ".repeat(right_pad as usize)
        ),
        Style::default()
            .fg(accent)
            .bg(MANTLE)
            .add_modifier(Modifier::BOLD),
    )]));

    f.render_widget(
        Paragraph::new(lines)
            .style(Style::default().bg(MANTLE))
            .alignment(Alignment::Left)
            .wrap(Wrap { trim: false }),
        inner,
    );
}

fn render_active_blade_border(f: &mut Frame, area: Rect, blade: Blade) -> Rect {
    fill(f, area, MANTLE);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(blade.accent()))
        .style(Style::default().bg(MANTLE));
    let inner = block.inner(area);
    f.render_widget(block, area);
    inner
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
    fn blade_widths_have_four_col_collapsed_and_fill_active() {
        let area = Rect::new(0, 0, 100, 40);
        for i in 0..Blade::count() {
            let rects = layout_blades(area, Blade::from_index(i));
            for (j, rect) in rects.iter().enumerate() {
                if i == j {
                    assert_eq!(rect.width, 84);
                } else {
                    assert_eq!(rect.width, 4);
                }
            }
        }
    }

    #[test]
    fn chrome_row_heights_are_one_each() {
        let layout = RootLayout::new(Blade::Inbox).compute(Rect::new(0, 0, 80, 24));
        assert_eq!(layout.body.height, 22);
        assert_eq!(layout.command_line.height, 1);
        assert_eq!(layout.keybar.height, 1);
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
