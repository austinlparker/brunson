use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::tui::app::AppState;
use crate::tui::render::layout::RootLayout;
use crate::tui::render::theme::{ADD, DEL, INBOX, MANTLE, OVERLAY0, SUBTEXT0, SURFACE0, TEXT};
use crate::tui::wizard::{self, SetupWizardState, WatchModeChoice, WizardStep};

/// Render the setup wizard, replacing the whole dashboard while it's open.
pub fn render_setup_wizard(f: &mut Frame, area: Rect, state: &AppState) {
    RootLayout::new(crate::tui::render::layout::Blade::Inbox).render(f, area);

    let Some(wizard) = state.setup_wizard.as_deref() else {
        return;
    };

    let block = Block::default()
        .title(" Brunson Setup ")
        .title_style(Style::default().fg(INBOX).add_modifier(Modifier::BOLD))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(INBOX))
        .style(Style::default().bg(MANTLE).fg(TEXT));

    let inner = block.inner(area);
    let content = inner.inner(ratatui::layout::Margin {
        horizontal: 2,
        vertical: 1,
    });

    f.render_widget(Clear, area);
    f.render_widget(block, area);

    let mut lines = match wizard.step {
        WizardStep::Welcome => render_welcome(wizard),
        WizardStep::AuthCheck => render_auth_check(wizard, state.ui_tick),
        WizardStep::WatchMode => render_watch_mode(wizard),
        WizardStep::WatchListInput => render_watch_list_input(wizard, content.width),
        WizardStep::TargetPicker => render_target_picker(wizard, state.ui_tick, content.width),
        WizardStep::TargetDetail => render_target_detail(wizard),
        WizardStep::LivePreview => render_live_preview(wizard, state.ui_tick),
        WizardStep::LlmConfig => render_llm_config(wizard, content.width),
        WizardStep::Confirm => render_confirm(wizard),
    };

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        wizard::step_keybar_hint(wizard.step),
        Style::default().fg(INBOX),
    )));

    let paragraph = Paragraph::new(lines)
        .alignment(Alignment::Left)
        .wrap(Wrap { trim: false })
        .style(Style::default().bg(MANTLE).fg(TEXT))
        .scroll((wizard.confirm_scroll as u16, 0));
    f.render_widget(paragraph, content);
}

fn title(text: &str) -> Line<'static> {
    Line::from(vec![Span::styled(
        text.to_string(),
        Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
    )])
}

fn dim(text: &str) -> Line<'static> {
    Line::from(vec![Span::styled(
        text.to_string(),
        Style::default().fg(OVERLAY0),
    )])
}

fn error_line(text: &str) -> Line<'static> {
    Line::from(vec![Span::styled(
        format!("✕ {text}"),
        Style::default().fg(DEL),
    )])
}

fn spinner(tick: u64) -> &'static str {
    const FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    FRAMES[(tick as usize) % FRAMES.len()]
}

fn selectable_line(label: String, selected: bool) -> Line<'static> {
    let prefix = if selected { "❯ " } else { "  " };
    let style = if selected {
        Style::default()
            .fg(TEXT)
            .bg(SURFACE0)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(SUBTEXT0)
    };
    Line::from(vec![Span::styled(format!("{prefix}{label}"), style)])
}

fn toggle_line(cursor: usize, row: usize, label: &str, enabled: bool) -> Line<'static> {
    let selected = cursor == row;
    let prefix = if selected { "❯ " } else { "  " };
    let mark = if enabled { "[x]" } else { "[ ]" };
    let mark_color = if enabled { ADD } else { OVERLAY0 };
    let label_style = if selected {
        Style::default().fg(TEXT).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(SUBTEXT0)
    };
    Line::from(vec![
        Span::styled(prefix.to_string(), label_style),
        Span::styled(format!("{mark} "), Style::default().fg(mark_color)),
        Span::styled(label.to_string(), label_style),
    ])
}

// ── Welcome ──

fn render_welcome(_wizard: &SetupWizardState) -> Vec<Line<'static>> {
    vec![
        title("Welcome to the brunson setup wizard."),
        Line::from(""),
        Line::from(
            "This will help you pick which GitHub PRs show up in your inbox, using your \
             real org/team memberships instead of typing them from memory.",
        ),
        Line::from(""),
        dim("Press Enter to begin."),
    ]
}

// ── AuthCheck ──

fn render_auth_check(wizard: &SetupWizardState, tick: u64) -> Vec<Line<'static>> {
    let mut lines = vec![title("GitHub authentication")];
    lines.push(Line::from(""));

    if wizard.auth.is_loading() {
        lines.push(Line::from(vec![
            Span::styled(spinner(tick), Style::default().fg(INBOX)),
            Span::raw(" checking..."),
        ]));
        return lines;
    }

    match wizard.auth.value() {
        Some(status) if status.auth.resolved && status.auth.user.is_some() => {
            lines.push(Line::from(vec![
                Span::styled("✓ ", Style::default().fg(ADD)),
                Span::raw(format!(
                    "Authenticated as {}",
                    status.auth.user.as_deref().unwrap_or("?")
                )),
            ]));
            lines.push(Line::from(""));
            lines.push(dim("Press Enter to continue."));
        }
        Some(status) => {
            lines.push(error_line("GitHub auth is not resolved."));
            for step in &status.next_steps {
                lines.push(Line::from(format!("  • {step}")));
            }
            lines.push(Line::from(""));
            lines.push(dim(
                "Run `gh auth login` or set GH_TOKEN in another terminal, then press r to recheck.",
            ));
        }
        None => {
            lines.push(dim("Press r to check auth status."));
        }
    }
    lines
}

// ── WatchMode ──

fn render_watch_mode(wizard: &SetupWizardState) -> Vec<Line<'static>> {
    let mut lines = vec![
        title("How should brunson pick which PRs to show?"),
        Line::from(""),
    ];
    for choice in [
        WatchModeChoice::Everything,
        WatchModeChoice::BroadWatch,
        WatchModeChoice::PreciseTargets,
    ] {
        lines.push(selectable_line(
            choice.label().to_string(),
            choice == wizard.watch_mode,
        ));
    }
    lines
}

// ── WatchListInput ──

fn render_watch_list_input(wizard: &SetupWizardState, width: u16) -> Vec<Line<'static>> {
    vec![
        title("Repositories/orgs to watch"),
        Line::from(""),
        dim("Comma-separated, e.g. myorg,myorg/important-repo"),
        Line::from(""),
        crate::tui::views::text::render_text_input_line(
            "> ",
            &wizard.watch_raw_input,
            width,
            true,
            Style::default().fg(SUBTEXT0),
            Style::default().fg(TEXT),
            Style::default().fg(INBOX),
        ),
    ]
}

// ── TargetPicker ──

fn render_target_picker(wizard: &SetupWizardState, tick: u64, width: u16) -> Vec<Line<'static>> {
    let mut lines = vec![title("Pick orgs to scope precise targets to")];
    lines.push(Line::from(""));

    if !wizard.selected_targets.is_empty() {
        lines.push(Line::from(Span::styled(
            "Configured targets:",
            Style::default().fg(INBOX).add_modifier(Modifier::BOLD),
        )));
        for target in &wizard.selected_targets {
            let scope = target
                .repo
                .clone()
                .or_else(|| target.org.clone())
                .unwrap_or_else(|| "?".to_string());
            lines.push(Line::from(format!("  • {scope}")));
        }
        lines.push(Line::from(""));
    }

    if wizard.manual_entry_active {
        lines.push(crate::tui::views::text::render_text_input_line(
            "Org or org/repo: ",
            &wizard.manual_entry_buffer,
            width,
            true,
            Style::default().fg(SUBTEXT0),
            Style::default().fg(TEXT),
            Style::default().fg(INBOX),
        ));
        return lines;
    }

    if wizard.memberships.is_loading() {
        lines.push(Line::from(vec![
            Span::styled(spinner(tick), Style::default().fg(INBOX)),
            Span::raw(" fetching your orgs/teams from GitHub..."),
        ]));
        return lines;
    }

    if let Some(err) = wizard.memberships.error() {
        lines.push(error_line(err));
        lines.push(Line::from(""));
    }

    match wizard.memberships.value() {
        Some(memberships) => {
            if memberships.truncated {
                lines.push(dim(
                    "Showing first 100 orgs/teams — press a to add one manually if yours is missing.",
                ));
            }
            for (i, org) in memberships.orgs.iter().enumerate() {
                let team_count = org.teams.len();
                let label = if team_count == 0 {
                    org.login.clone()
                } else {
                    format!(
                        "{} ({} team{})",
                        org.login,
                        team_count,
                        if team_count == 1 { "" } else { "s" }
                    )
                };
                lines.push(selectable_line(label, wizard.target_cursor == i));
            }
            lines.push(selectable_line(
                "+ Add manually (org or org/repo)".to_string(),
                wizard.target_cursor == memberships.orgs.len(),
            ));
        }
        None => {
            lines.push(selectable_line(
                "+ Add manually (org or org/repo)".to_string(),
                true,
            ));
        }
    }
    lines
}

// ── TargetDetail ──

fn render_target_detail(wizard: &SetupWizardState) -> Vec<Line<'static>> {
    let Some(target) = wizard.editing_target.as_ref() else {
        return vec![dim("Nothing being edited.")];
    };
    let scope = target
        .repo
        .clone()
        .or_else(|| target.org.clone())
        .unwrap_or_else(|| "?".to_string());

    let mut lines = vec![title(&format!("Configure target: {scope}")), Line::from("")];

    let cursor = wizard.editing_target_cursor;
    lines.push(toggle_line(
        cursor,
        0,
        "Direct review requests (user-review-requested:@me)",
        target.direct_review_requests,
    ));
    lines.push(toggle_line(
        cursor,
        1,
        "PRs I authored here",
        target.include_authored,
    ));
    lines.push(toggle_line(
        cursor,
        2,
        "PRs I'm otherwise involved in",
        target.include_involved,
    ));

    let teams = wizard.available_teams();
    if !teams.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Team review requests:",
            Style::default().fg(INBOX).add_modifier(Modifier::BOLD),
        )));
        for (i, team) in teams.iter().enumerate() {
            let team_id = format!("{}/{}", target.org.clone().unwrap_or_default(), team.slug);
            let enabled = target.team_review_requests.contains(&team_id);
            lines.push(toggle_line(cursor, 3 + i, &team.name, enabled));
        }
    }

    if let Some(err) = &wizard.target_error {
        lines.push(Line::from(""));
        lines.push(error_line(err));
    }

    lines
}

// ── LivePreview ──

fn render_live_preview(wizard: &SetupWizardState, tick: u64) -> Vec<Line<'static>> {
    let mut lines = vec![title("Here's what you'll see"), Line::from("")];

    if wizard.preview.is_loading() {
        lines.push(Line::from(vec![
            Span::styled(spinner(tick), Style::default().fg(INBOX)),
            Span::raw(" running your queries against GitHub..."),
        ]));
        return lines;
    }

    if let Some(err) = wizard.preview.error() {
        lines.push(error_line(err));
        return lines;
    }

    match wizard.preview.value() {
        Some(preview) => {
            lines.push(Line::from(vec![
                Span::styled(
                    preview.total_matched_prs.to_string(),
                    Style::default().fg(ADD).add_modifier(Modifier::BOLD),
                ),
                Span::raw(" distinct open PR(s) would show up in your inbox."),
            ]));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Queries:",
                Style::default().fg(INBOX).add_modifier(Modifier::BOLD),
            )));
            for query in &preview.queries {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(query.clone(), Style::default().fg(SUBTEXT0)),
                ]));
            }
            for err in &preview.errors {
                lines.push(error_line(err));
            }
        }
        None => lines.push(dim("Press r to fetch a live preview.")),
    }
    lines
}

// ── LlmConfig ──

fn render_llm_config(wizard: &SetupWizardState, width: u16) -> Vec<Line<'static>> {
    let mut lines = vec![title("LLM classification (optional)"), Line::from("")];
    let cursor = wizard.llm_cursor;

    lines.push(toggle_line(
        cursor,
        0,
        "Enable LLM classification",
        wizard.llm_enabled,
    ));
    if !wizard.llm_enabled {
        return lines;
    }

    let provider = if wizard.llm_provider_idx == 0 {
        "lm_studio (local)"
    } else {
        "openai_compatible"
    };
    lines.push(selectable_line(
        format!("Provider: {provider}"),
        cursor == 1,
    ));
    lines.push(crate::tui::views::text::render_text_input_line(
        "Endpoint: ",
        &wizard.llm_endpoint,
        width,
        cursor == 2 && wizard.llm_editing_field,
        Style::default().fg(SUBTEXT0),
        Style::default().fg(TEXT),
        Style::default().fg(INBOX),
    ));
    let masked_key = if wizard.llm_api_key.is_empty() {
        String::new()
    } else {
        "*".repeat(wizard.llm_api_key.len())
    };
    lines.push(crate::tui::views::text::render_text_input_line(
        "API key: ",
        &masked_key,
        width,
        cursor == 3 && wizard.llm_editing_field,
        Style::default().fg(SUBTEXT0),
        Style::default().fg(TEXT),
        Style::default().fg(INBOX),
    ));
    lines.push(crate::tui::views::text::render_text_input_line(
        "Model (empty = auto-detect): ",
        &wizard.llm_model,
        width,
        cursor == 4 && wizard.llm_editing_field,
        Style::default().fg(SUBTEXT0),
        Style::default().fg(TEXT),
        Style::default().fg(INBOX),
    ));
    lines
}

// ── Confirm ──

fn render_confirm(wizard: &SetupWizardState) -> Vec<Line<'static>> {
    let mut lines = vec![title("Review and confirm"), Line::from("")];
    let draft = wizard.draft();
    match toml::to_string_pretty(&draft) {
        Ok(text) => {
            for line in text.lines() {
                lines.push(Line::from(Span::styled(
                    line.to_string(),
                    Style::default().fg(SUBTEXT0),
                )));
            }
        }
        Err(e) => lines.push(error_line(&format!("Failed to render config: {e}"))),
    }

    // Previously these were two independent `if`s over separate
    // `commit_loading`/`commit_error` fields, so a retry after a failed
    // commit could show the "writing..." spinner *and* the stale error
    // from the previous attempt at the same time. The enum makes that
    // combination unrepresentable: a resource is Loading or Failed, never
    // both.
    if wizard.commit.is_loading() {
        lines.push(Line::from(""));
        lines.push(dim("Writing config and reloading daemon..."));
    }
    if let Some(err) = wizard.commit.error() {
        lines.push(Line::from(""));
        lines.push(error_line(err));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use crate::config::Config;
    use crate::tui::client::DaemonClient;
    use crate::tui::wizard::SetupWizardState;

    fn state_with_wizard(step: WizardStep) -> AppState {
        let mut state = AppState::new(Config::default(), DaemonClient::new(17890).unwrap());
        let mut wizard =
            SetupWizardState::hydrate(std::path::PathBuf::from("/tmp/x.toml"), &Config::default());
        wizard.step = step;
        state.setup_wizard = Some(Box::new(wizard));
        state
    }

    #[test]
    fn every_step_renders_without_panic() {
        let steps = [
            WizardStep::Welcome,
            WizardStep::AuthCheck,
            WizardStep::WatchMode,
            WizardStep::WatchListInput,
            WizardStep::TargetPicker,
            WizardStep::TargetDetail,
            WizardStep::LivePreview,
            WizardStep::LlmConfig,
            WizardStep::Confirm,
        ];
        for step in steps {
            let backend = TestBackend::new(100, 30);
            let mut terminal = Terminal::new(backend).unwrap();
            let mut state = state_with_wizard(step);
            if step == WizardStep::TargetDetail {
                state.setup_wizard.as_mut().unwrap().editing_target =
                    Some(crate::config::GithubTarget {
                        org: Some("myorg".to_string()),
                        repo: None,
                        direct_review_requests: true,
                        team_review_requests: vec![],
                        include_authored: true,
                        include_involved: false,
                    });
            }
            terminal
                .draw(|f| render_setup_wizard(f, f.area(), &state))
                .unwrap();
        }
    }

    #[test]
    fn renders_nothing_when_wizard_closed() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let state = AppState::new(Config::default(), DaemonClient::new(17890).unwrap());
        assert!(state.setup_wizard.is_none());
        terminal
            .draw(|f| render_setup_wizard(f, f.area(), &state))
            .unwrap();
    }
}
