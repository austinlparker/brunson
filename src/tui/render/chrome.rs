use ratatui::layout::{Alignment, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};
use ratatui::Frame;

use super::component::{Component, RenderContext};
use super::layout::fill;
use super::theme::{FAIL, MANTLE, PENDING, TEXT};

/// Centered overlay layer that renders `state.error_message` over the body.
/// The keybar is never replaced by this toast; it is drawn on top of the body
/// area only.
#[derive(Debug, Clone, Copy, Default)]
pub struct InlineToast;

impl Component for InlineToast {
    fn render(&self, f: &mut Frame, area: Rect, ctx: &RenderContext) {
        let Some(message) = ctx
            .state
            .error_message
            .as_deref()
            .or(ctx.state.transient_message.as_deref())
        else {
            return;
        };
        if area.width < 8 || area.height < 3 {
            return;
        }

        let kind = if message.starts_with("Help:") {
            ToastKind::Help
        } else if message.to_ascii_lowercase().contains("failed")
            || message.to_ascii_lowercase().contains("error")
        {
            ToastKind::Error
        } else {
            ToastKind::Info
        };
        let color = match kind {
            ToastKind::Error => FAIL,
            ToastKind::Info => PENDING,
            ToastKind::Help => ctx.view.active_blade.accent(),
        };

        let max_w = area.width.saturating_sub(4).clamp(8, 72);
        let wrapped = textwrap::wrap(message, max_w.saturating_sub(4) as usize);
        let h = (wrapped.len() as u16 + 2)
            .min(area.height.saturating_sub(2))
            .max(3);
        let rect = centered_rect(area, max_w, h);

        f.render_widget(Clear, rect);
        fill(f, rect, MANTLE);
        let block = ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .border_style(Style::default().fg(color).bg(MANTLE))
            .style(Style::default().bg(MANTLE));
        let inner = block.inner(rect);
        f.render_widget(block, rect);

        let lines: Vec<Line> = wrapped
            .into_iter()
            .map(|line| {
                Line::from(Span::styled(
                    line.into_owned(),
                    Style::default().fg(TEXT).bg(MANTLE),
                ))
            })
            .collect();
        f.render_widget(
            Paragraph::new(lines)
                .style(Style::default().bg(MANTLE))
                .alignment(Alignment::Center)
                .wrap(Wrap { trim: false }),
            inner,
        );
    }
}

#[derive(Debug, Clone, Copy)]
enum ToastKind {
    Error,
    Info,
    Help,
}

fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect::new(
        area.x + area.width.saturating_sub(w) / 2,
        area.y + area.height.saturating_sub(h) / 2,
        w,
        h,
    )
}
