use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::github::search::build_queries_for_config;
use crate::tui::render::component::RenderContext;
use crate::tui::render::layout::fill;
use crate::tui::render::theme::{BASE, INBOX, MANTLE, OVERLAY0, SUBTEXT0, SURFACE0, TEXT};

pub fn render_config_view(f: &mut Frame, area: Rect, ctx: &RenderContext) {
    let popup = config_rect(area);
    f.render_widget(Clear, popup);
    fill(f, popup, BASE);

    let block = Block::default()
        .title(" Config ")
        .title_alignment(Alignment::Left)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(INBOX))
        .style(Style::default().fg(TEXT).bg(MANTLE));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let lines = config_lines(ctx);
    let max_scroll = lines.len().saturating_sub(inner.height as usize);
    let offset = ctx.state.config_scroll.min(max_scroll);
    let visible = lines
        .into_iter()
        .skip(offset)
        .take(inner.height as usize)
        .collect::<Vec<_>>();

    f.render_widget(
        Paragraph::new(visible)
            .style(Style::default().fg(TEXT).bg(MANTLE))
            .wrap(Wrap { trim: false }),
        inner,
    );
}

fn config_rect(area: Rect) -> Rect {
    let margin_x = if area.width >= 100 { 8 } else { 2 };
    let margin_y = if area.height >= 30 { 3 } else { 1 };
    Rect {
        x: area.x.saturating_add(margin_x),
        y: area.y.saturating_add(margin_y),
        width: area.width.saturating_sub(margin_x * 2).max(1),
        height: area.height.saturating_sub(margin_y * 2).max(1),
    }
}

fn config_lines(ctx: &RenderContext) -> Vec<Line<'static>> {
    let config = &ctx.state.config;
    let github = &config.github;
    let mut lines = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(
            "Configuration",
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "  c/Esc close  R reload  j/k scroll",
            Style::default().fg(SUBTEXT0),
        ),
    ]));
    lines.push(Line::from(""));

    section(&mut lines, "Runtime status");
    let config_path = ctx
        .state
        .config_path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "default".to_string());
    lines.push(label_value("config path", &config_path));
    if let Some(health) = &ctx.state.health {
        lines.push(label_value("service", &health.service));
        lines.push(label_value("version", &health.version));
        lines.push(label_value("daemon status", &health.status));
        lines.push(label_value("current user", &health.current_user));
        lines.push(label_value(
            "refresh in progress",
            bool_label(health.refresh_in_progress),
        ));
        lines.push(label_value(
            "last poll at",
            health.last_poll_at.as_deref().unwrap_or("never"),
        ));
        lines.push(label_value(
            "last poll error",
            health.last_poll_error.as_deref().unwrap_or("none"),
        ));
        lines.push(label_value(
            "rate limit remaining",
            &health
                .rate_limit_remaining
                .map(|n| n.to_string())
                .unwrap_or_else(|| "unknown".to_string()),
        ));
        lines.push(label_value("setup status", &health.setup_status));
        lines.push(label_value(
            "setup message",
            health.setup_message.as_deref().unwrap_or("none"),
        ));
    } else {
        lines.push(dim_line("health not loaded"));
    }

    lines.push(Line::from(""));
    section(&mut lines, "Setup diagnostics");
    if let Some(setup) = &ctx.state.setup_status {
        lines.push(label_value("ready", bool_label(setup.ready)));
        lines.push(label_value("status", &setup.status));
        lines.push(label_value(
            "auth resolved",
            bool_label(setup.auth.resolved),
        ));
        lines.push(label_value(
            "auth source",
            setup.auth.source.as_deref().unwrap_or("none"),
        ));
        lines.push(label_value(
            "auth user",
            setup.auth.user.as_deref().unwrap_or("none"),
        ));
        lines.push(label_value("llm enabled", bool_label(setup.llm.enabled)));
        lines.push(label_value(
            "llm reachable",
            setup.llm.reachable.map(bool_label).unwrap_or("unknown"),
        ));
        lines.push(label_value(
            "llm model",
            setup.llm.model.as_deref().unwrap_or("none"),
        ));
        lines.push(label_value(
            "llm message",
            setup.llm.message.as_deref().unwrap_or("none"),
        ));
        if setup.next_steps.is_empty() {
            lines.push(label_value("next steps", "none"));
        } else {
            lines.push(label_only("next steps"));
            for step in &setup.next_steps {
                lines.push(indent_value(step));
            }
        }
    } else {
        lines.push(dim_line("setup diagnostics not loaded"));
    }

    lines.push(Line::from(""));
    section(&mut lines, "GitHub config");
    lines.push(label_value(
        "poll interval",
        &format!("{}s", github.poll_interval),
    ));

    if github.watch.is_empty() {
        lines.push(label_value(
            "broad watch",
            if github.targets.is_empty() {
                "all PRs involving @me"
            } else {
                "disabled; using explicit targets"
            },
        ));
    } else {
        lines.push(Line::from(vec![Span::styled(
            "Broad watch rules",
            Style::default().fg(INBOX).add_modifier(Modifier::BOLD),
        )]));
        for entry in &github.watch {
            let scope = if entry.contains('/') { "repo" } else { "org" };
            lines.push(Line::from(vec![
                Span::styled("  ", Style::default().fg(TEXT)),
                Span::styled(scope.to_string(), Style::default().fg(SUBTEXT0)),
                Span::styled(" ", Style::default().fg(SUBTEXT0)),
                Span::styled(entry.clone(), Style::default().fg(TEXT)),
                Span::styled(
                    " includes review-requested:@me teams",
                    Style::default().fg(OVERLAY0),
                ),
            ]));
        }
    }

    lines.push(Line::from(""));
    section(&mut lines, "Explicit targets");

    if github.targets.is_empty() {
        lines.push(dim_line("  none"));
    } else {
        for (i, target) in github.targets.iter().enumerate() {
            let scope = target
                .repo
                .as_ref()
                .map(|repo| format!("repo:{repo}"))
                .or_else(|| target.org.as_ref().map(|org| format!("org:{org}")))
                .unwrap_or_else(|| "invalid".to_string());
            lines.push(Line::from(vec![
                Span::styled(format!("  {}. ", i + 1), Style::default().fg(SUBTEXT0)),
                Span::styled(
                    scope,
                    Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                ),
            ]));
            lines.push(Line::from(vec![
                Span::styled("     direct ", Style::default().fg(SUBTEXT0)),
                Span::styled(
                    on_off(target.direct_review_requests),
                    toggle_style(target.direct_review_requests),
                ),
                Span::styled("  authored ", Style::default().fg(SUBTEXT0)),
                Span::styled(
                    on_off(target.include_authored),
                    toggle_style(target.include_authored),
                ),
                Span::styled("  involved ", Style::default().fg(SUBTEXT0)),
                Span::styled(
                    on_off(target.include_involved),
                    toggle_style(target.include_involved),
                ),
            ]));
            let teams = if target.team_review_requests.is_empty() {
                "none".to_string()
            } else {
                target.team_review_requests.join(", ")
            };
            lines.push(label_value("     teams", &teams));
        }
    }

    lines.push(Line::from(""));
    section(&mut lines, "Daemon config");
    lines.push(label_value("port", &config.daemon.port.to_string()));
    lines.push(label_value(
        "kill on tui exit",
        bool_label(config.daemon.kill_on_tui_exit),
    ));

    lines.push(Line::from(""));
    section(&mut lines, "LLM config");
    lines.push(label_value("enabled", bool_label(config.llm.enabled)));
    lines.push(label_value("provider", &config.llm.provider));
    lines.push(label_value("endpoint", &config.llm.endpoint));
    lines.push(label_value(
        "api key",
        if config.llm.api_key.is_empty() {
            "empty"
        } else {
            "redacted"
        },
    ));
    lines.push(label_value(
        "model",
        if config.llm.model.is_empty() {
            "auto-detect"
        } else {
            &config.llm.model
        },
    ));
    lines.push(label_value(
        "classify on change",
        bool_label(config.llm.classify_on_change),
    ));
    lines.push(label_value(
        "max output tokens",
        &config.llm.max_output_tokens.to_string(),
    ));

    lines.push(Line::from(""));
    section(&mut lines, "TUI config");
    lines.push(label_value(
        "show line numbers",
        bool_label(config.tui.show_line_numbers),
    ));

    lines.push(Line::from(""));
    section(&mut lines, "Generated queries");
    let queries = build_queries_for_config(github);
    if queries.is_empty() {
        lines.push(dim_line("  none"));
    } else {
        for query in queries {
            lines.push(Line::from(vec![
                Span::styled("  ", Style::default().fg(TEXT)),
                Span::styled(query, Style::default().fg(SUBTEXT0)),
            ]));
        }
    }

    lines
}

fn section(lines: &mut Vec<Line<'static>>, title: &str) {
    lines.push(Line::from(vec![Span::styled(
        title.to_string(),
        Style::default().fg(INBOX).add_modifier(Modifier::BOLD),
    )]));
}

fn label_value(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label}: "), Style::default().fg(SUBTEXT0)),
        Span::styled(value.to_string(), Style::default().fg(TEXT)),
    ])
}

fn label_only(label: &str) -> Line<'static> {
    Line::from(vec![Span::styled(
        format!("{label}:"),
        Style::default().fg(SUBTEXT0),
    )])
}

fn indent_value(value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled("  ", Style::default().fg(TEXT)),
        Span::styled(value.to_string(), Style::default().fg(TEXT)),
    ])
}

fn dim_line(value: &str) -> Line<'static> {
    Line::from(vec![Span::styled(
        value.to_string(),
        Style::default().fg(OVERLAY0),
    )])
}

fn bool_label(value: bool) -> &'static str {
    if value {
        "true"
    } else {
        "false"
    }
}

fn on_off(enabled: bool) -> &'static str {
    if enabled {
        "on"
    } else {
        "off"
    }
}

fn toggle_style(enabled: bool) -> Style {
    if enabled {
        Style::default()
            .fg(TEXT)
            .bg(SURFACE0)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(OVERLAY0)
    }
}
