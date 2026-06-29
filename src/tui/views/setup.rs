use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::tui::app::AppState;
use crate::tui::render::layout::RootLayout;

const TEXT: Color = Color::Rgb(205, 214, 244);
const INBOX: Color = Color::Rgb(137, 180, 250);
const OVERLAY: Color = Color::Rgb(49, 50, 68);

/// Render a first-run setup overlay when the daemon reports it is not ready.
pub fn render_setup_wizard(f: &mut Frame, area: Rect, state: &AppState) {
    // Fill the entire terminal background.
    RootLayout::new(crate::tui::render::layout::Blade::Inbox).render(f, area);

    let block = Block::default()
        .title(" Brunson Setup ")
        .title_style(Style::default().fg(INBOX).add_modifier(Modifier::BOLD))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(INBOX))
        .style(Style::default().bg(OVERLAY).fg(TEXT));

    let inner = block.inner(area);
    // Keep a margin so text never touches the border.
    let content = inner.inner(ratatui::layout::Margin {
        horizontal: 2,
        vertical: 1,
    });

    f.render_widget(Clear, area);
    f.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();

    lines.push(Line::from(vec![Span::styled(
        "brunson needs a little configuration before it can show your PRs.",
        Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
    )]));
    lines.push(Line::from(""));

    if let Some(ref setup) = state.setup_status {
        lines.push(Line::from(vec![
            Span::raw("Status: "),
            Span::styled(
                &setup.status,
                Style::default().fg(if setup.ready {
                    Color::Green
                } else {
                    Color::Yellow
                }),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::raw("GitHub auth: "),
            Span::styled(
                if setup.auth.resolved {
                    if let Some(ref user) = setup.auth.user {
                        format!("resolved ({user})")
                    } else {
                        "token present, login failed".to_string()
                    }
                } else {
                    "missing".to_string()
                },
                Style::default().fg(if setup.auth.resolved {
                    Color::Green
                } else {
                    Color::Yellow
                }),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::raw("LLM: "),
            Span::styled(
                if !setup.llm.enabled {
                    "disabled".to_string()
                } else if setup.llm.reachable == Some(true) {
                    if let Some(ref m) = setup.llm.model {
                        format!("reachable ({m})")
                    } else {
                        "reachable".to_string()
                    }
                } else {
                    "misconfigured".to_string()
                },
                Style::default().fg(if !setup.llm.enabled || setup.llm.reachable == Some(true) {
                    Color::Green
                } else {
                    Color::Yellow
                }),
            ),
        ]));

        if !setup.next_steps.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Next steps:",
                Style::default().add_modifier(Modifier::BOLD),
            )));
            for step in &setup.next_steps {
                lines.push(Line::from(format!("  • {}", step)));
            }
        }
    } else {
        lines.push(Line::from("Could not fetch setup status from the daemon."));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Keys: s run setup  |  R reload config  |  q quit",
        Style::default().fg(INBOX),
    )));

    let paragraph = Paragraph::new(lines)
        .alignment(Alignment::Left)
        .wrap(Wrap { trim: false })
        .style(Style::default().bg(OVERLAY).fg(TEXT));
    f.render_widget(paragraph, content);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use crate::api::SetupStatusResponse;
    use crate::config::Config;
    use crate::tui::client::DaemonClient;

    #[test]
    fn setup_overlay_renders_without_panic() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new(Config::default(), DaemonClient::new(17890).unwrap());
        state.setup_status = Some(SetupStatusResponse::default());
        state.show_setup_wizard = true;
        terminal
            .draw(|f| render_setup_wizard(f, f.area(), &state))
            .unwrap();
    }
}
