use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::tui::render::component::RenderContext;
use crate::tui::render::layout::fill;
use crate::tui::render::theme::{BASE, INBOX, MANTLE, SUBTEXT0, TEXT};

/// Render the inbox filter search overlay anchored just above the keybar.
pub fn render_search_overlay(f: &mut Frame, area: Rect, ctx: &RenderContext) {
    let popup = search_rect(area);
    f.render_widget(Clear, popup);
    fill(f, popup, BASE);

    let block = Block::default()
        .title(" Filter ")
        .title_alignment(Alignment::Left)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(INBOX))
        .style(Style::default().fg(TEXT).bg(MANTLE));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let input_line = crate::tui::views::text::render_text_input_line(
        "Filter: ",
        &ctx.state.search_filter,
        inner.width,
        true,
        Style::default().fg(SUBTEXT0),
        Style::default().fg(TEXT),
        Style::default().fg(INBOX),
    );

    let hint_line = Line::from(vec![Span::styled(
        "Enter accept · Esc cancel · Backspace delete",
        Style::default().fg(SUBTEXT0).add_modifier(Modifier::DIM),
    )]);

    f.render_widget(
        Paragraph::new(vec![input_line, Line::from(""), hint_line])
            .style(Style::default().bg(MANTLE)),
        inner,
    );
}

fn search_rect(area: Rect) -> Rect {
    let margin_x = if area.width >= 100 { 8 } else { 2 };
    let height = 5u16;
    let y = area.y.saturating_add(area.height.saturating_sub(height));
    Rect {
        x: area.x.saturating_add(margin_x),
        y,
        width: area.width.saturating_sub(margin_x * 2).max(1),
        height: height.min(area.height),
    }
}
