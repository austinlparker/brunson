use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::tui::app::StartupPhase;
use crate::tui::render::component::RenderContext;
use crate::tui::render::theme::{INBOX, MANTLE, OVERLAY0, SUBTEXT0, TEXT};

pub fn render_loading(f: &mut Frame, area: Rect, ctx: &RenderContext) {
    let popup = centered_rect(area, 52, 8);
    f.render_widget(Clear, popup);

    let block = Block::default()
        .title(" Brunson ")
        .title_alignment(Alignment::Left)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(INBOX))
        .style(Style::default().fg(TEXT).bg(MANTLE));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let spinner = spinner(ctx.state.ui_tick);
    let phase = phase_label(ctx.state.startup_phase);
    let bar = progress_bar(ctx.state.ui_tick, inner.width.saturating_sub(4) as usize);
    let lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled(
                spinner,
                Style::default().fg(INBOX).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" ", Style::default().fg(TEXT)),
            Span::styled(
                phase,
                Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(""),
        Line::from(vec![Span::styled(bar, Style::default().fg(INBOX))]),
        Line::from(vec![Span::styled(
            "Starting daemon, checking setup, and loading PRs",
            Style::default().fg(SUBTEXT0),
        )]),
        Line::from(vec![Span::styled(
            "Press q to quit",
            Style::default().fg(OVERLAY0),
        )]),
    ];

    f.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .style(Style::default().fg(TEXT).bg(MANTLE)),
        inner,
    );
}

fn spinner(tick: u64) -> &'static str {
    match tick % 4 {
        0 => "|",
        1 => "/",
        2 => "-",
        _ => "\\",
    }
}

fn phase_label(phase: StartupPhase) -> &'static str {
    match phase {
        StartupPhase::StartingDaemon => "Starting daemon",
        StartupPhase::CheckingSetup => "Checking setup",
        StartupPhase::LoadingConfig => "Loading config",
        StartupPhase::LoadingPrs => "Loading pull requests",
        StartupPhase::Ready => "Ready",
    }
}

fn progress_bar(tick: u64, width: usize) -> String {
    let width = width.clamp(12, 44);
    let window = 7usize.min(width);
    let pos = (tick as usize) % width;
    let mut chars = vec!['-'; width];
    for i in 0..window {
        chars[(pos + i) % width] = '#';
    }
    format!("[{}]", chars.into_iter().collect::<String>())
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
    use crate::config::Config;
    use crate::tui::client::DaemonClient;
    use crate::tui::render::component::RenderContext;
    use crate::tui::render::theme::Theme;
    use crate::tui::state::ViewState;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn loading_overlay_renders_without_panic() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state =
            crate::tui::app::AppState::new(Config::default(), DaemonClient::new(17890).unwrap());
        state.startup_phase = StartupPhase::LoadingPrs;
        state.ui_tick = 2;
        let view = ViewState::default();
        let theme = Theme;
        terminal
            .draw(|f| render_loading(f, f.area(), &RenderContext::new(&state, &view, &theme)))
            .unwrap();
    }

    #[test]
    fn progress_bar_keeps_stable_width() {
        let bar = progress_bar(3, 20);
        assert_eq!(bar.chars().count(), 22);
        assert!(bar.starts_with('['));
        assert!(bar.ends_with(']'));
    }
}
