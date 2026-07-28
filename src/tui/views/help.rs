use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::tui::render::component::RenderContext;
use crate::tui::render::layout::fill;
use crate::tui::render::theme::{BASE, INBOX, MANTLE, SUBTEXT0, TEXT};

/// Render the keybinding help modal: a centered overlay with a two-column
/// key/action table, grouped by scope (global, then per blade, then the filter
/// overlay). Styled like the config overlay and scrolled via `help_scroll`.
pub fn render_help(f: &mut Frame, area: Rect, ctx: &RenderContext) {
    let popup = help_rect(area);
    f.render_widget(Clear, popup);
    fill(f, popup, BASE);

    let block = Block::default()
        .title(" Help ")
        .title_alignment(Alignment::Left)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(INBOX))
        .style(Style::default().fg(TEXT).bg(MANTLE));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let lines = help_lines();
    let max_scroll = lines.len().saturating_sub(inner.height as usize);
    let offset = ctx.state.help_scroll.min(max_scroll);
    let visible = lines
        .into_iter()
        .skip(offset)
        .take(inner.height as usize)
        .collect::<Vec<_>>();

    f.render_widget(
        Paragraph::new(visible).style(Style::default().fg(TEXT).bg(MANTLE)),
        inner,
    );
}

/// A centered ~70×24 rectangle, clamped to fit inside `area`.
fn help_rect(area: Rect) -> Rect {
    let width = 70u16.min(area.width.saturating_sub(4).max(1));
    let height = 24u16.min(area.height.saturating_sub(2).max(1));
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect {
        x,
        y,
        width,
        height,
    }
}

fn help_lines() -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    lines.push(Line::from(Span::styled(
        "j/k scroll · ? / esc / q close",
        Style::default().fg(SUBTEXT0).add_modifier(Modifier::DIM),
    )));
    lines.push(Line::from(""));

    section(&mut lines, "Global");
    for (k, a) in [
        ("q", "quit"),
        ("r / R", "refresh"),
        ("c", "config"),
        ("w", "setup wizard"),
        ("o", "open in browser"),
        ("/", "filter inbox"),
        ("1–5", "jump to blade"),
        ("← → / h l", "move between blades"),
        ("esc", "step out / back"),
        ("?", "toggle this help"),
    ] {
        lines.push(binding(k, a));
    }
    lines.push(Line::from(""));

    section(&mut lines, "Inbox");
    for (k, a) in [
        ("j / k", "move cursor"),
        ("g / G", "first / last"),
        ("^d / ^u", "half-page"),
        ("space", "fold section"),
        ("⏎", "open PR / fold header"),
    ] {
        lines.push(binding(k, a));
    }
    lines.push(Line::from(""));

    section(&mut lines, "Overview");
    for (k, a) in [
        ("tab / S-tab", "cycle section"),
        ("d", "expand description"),
        ("j / k", "scroll section"),
    ] {
        lines.push(binding(k, a));
    }
    lines.push(Line::from(""));

    section(&mut lines, "Activity");
    for (k, a) in [
        ("j / k", "scroll"),
        ("g / G", "top / bottom"),
        ("^d / ^u", "half-page"),
    ] {
        lines.push(binding(k, a));
    }
    lines.push(Line::from(""));

    section(&mut lines, "Files");
    for (k, a) in [("j / k", "move"), ("⏎", "view diff")] {
        lines.push(binding(k, a));
    }
    lines.push(Line::from(""));

    section(&mut lines, "Diff");
    for (k, a) in [
        ("n", "line numbers"),
        ("tab / S-tab", "prev / next file"),
        ("g / G", "top / bottom"),
        ("^d / ^u", "page"),
    ] {
        lines.push(binding(k, a));
    }
    lines.push(Line::from(""));

    section(&mut lines, "Filter overlay");
    for (k, a) in [("⏎", "accept"), ("esc", "cancel"), ("⌫", "delete char")] {
        lines.push(binding(k, a));
    }

    lines
}

fn section(lines: &mut Vec<Line<'static>>, title: &str) {
    lines.push(Line::from(Span::styled(
        title.to_string(),
        Style::default().fg(INBOX).add_modifier(Modifier::BOLD),
    )));
}

/// One key/action row: the key column padded to a fixed width, then its action.
fn binding(key: &str, action: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("  {:<12}", key),
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(action.to_string(), Style::default().fg(SUBTEXT0)),
    ])
}
