use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::app::{App, Focus, TimelineItem};

use crate::{editor::display, theme::Theme};

pub fn draw(frame: &mut Frame<'_>, app: &App, model: &str) {
    let area = frame.area();
    frame.render_widget(
        Block::default().style(
            Style::default()
                .fg(app.theme.foreground)
                .bg(app.theme.background),
        ),
        area,
    );
    if area.width < 60 || area.height < 16 {
        frame.render_widget(
            Paragraph::new("Terminal too small\nMinimum: 60 x 16")
                .alignment(Alignment::Center)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(border(app.theme, false)),
                ),
            area,
        );
        return;
    }

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(8),
            Constraint::Length(
                (app.input
                    .view(usize::from(area.width.saturating_sub(2)))
                    .lines
                    .len() as u16
                    + 2)
                .clamp(3, area.height.saturating_sub(10).clamp(3, 10)),
            ),
            Constraint::Length(2),
        ])
        .split(area);
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(28), Constraint::Min(30)])
        .split(vertical[0]);

    draw_sessions(frame, app, body[0]);
    draw_timeline(frame, app, body[1]);
    draw_input(frame, app, vertical[1]);
    draw_status(frame, app, model, vertical[2]);
    draw_slash_menu(frame, app, vertical[1]);
    if app.approval.is_some() {
        draw_approval(frame, app, area);
    } else if app.confirm_delete {
        draw_delete_confirmation(frame, app, area);
    }
}

fn draw_slash_menu(frame: &mut Frame<'_>, app: &App, input_area: Rect) {
    let suggestions = app.slash_suggestions();
    if suggestions.is_empty() {
        return;
    }

    let height = u16::try_from(suggestions.len()).unwrap_or(u16::MAX).min(6) + 2;
    let area = Rect {
        x: input_area.x,
        y: input_area.y.saturating_sub(height),
        width: input_area.width.min(72),
        height,
    };
    let items = suggestions.iter().enumerate().map(|(index, spec)| {
        let selected = index == app.slash_selection();
        let style = if selected {
            Style::default()
                .fg(app.theme.background)
                .bg(app.theme.accent)
        } else {
            Style::default()
        };
        ListItem::new(Line::from(vec![
            Span::styled(
                format!(" {:<14}", spec.usage),
                style.add_modifier(Modifier::BOLD),
            ),
            Span::styled(spec.description, style),
        ]))
        .style(style)
    });
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::default().style(
            Style::default()
                .fg(app.theme.foreground)
                .bg(app.theme.background),
        ),
        area,
    );
    let mut state = ListState::default().with_selected(Some(app.slash_selection()));
    frame.render_stateful_widget(
        List::new(items).block(
            Block::default()
                .title(" Commands ")
                .title_bottom(
                    Line::from(" Up/Down select  Enter run  Tab complete  Esc close ")
                        .right_aligned(),
                )
                .borders(Borders::ALL)
                .border_style(border(app.theme, true)),
        ),
        area,
        &mut state,
    );
}

fn draw_sessions(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let items = if app.sessions.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            "No sessions",
            Style::default().fg(app.theme.muted),
        )))]
    } else {
        app.sessions
            .iter()
            .enumerate()
            .map(|(index, session)| {
                let selected = index == app.selected_session;
                let marker = if selected { "> " } else { "  " };
                let title = session.title.as_deref().unwrap_or("Untitled");
                ListItem::new(vec![
                    Line::from(vec![
                        Span::styled(marker, Style::default().fg(app.theme.accent)),
                        Span::styled(
                            truncate(&display(title), 22),
                            Style::default().add_modifier(if selected {
                                Modifier::BOLD
                            } else {
                                Modifier::empty()
                            }),
                        ),
                    ]),
                    Line::from(Span::styled(
                        format!("  {} messages", session.message_count),
                        Style::default().fg(app.theme.muted),
                    )),
                ])
            })
            .collect()
    };
    frame.render_widget(
        List::new(items).block(
            Block::default()
                .title(" Sessions ")
                .title_bottom(Line::from(" n new  d delete  Enter open ").right_aligned())
                .borders(Borders::ALL)
                .border_style(border(app.theme, app.focus == Focus::Sessions)),
        ),
        area,
    );
}

fn draw_timeline(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let mut lines = Vec::new();
    if app.preview_omitted {
        lines.push(Line::from(
            "[Display preview bounded; inspect available canonical records]",
        ));
    }
    if app.timeline.is_empty() {
        lines.push(Line::from(Span::styled(
            "Start a conversation from the input below.",
            Style::default().fg(app.theme.muted),
        )));
    }
    for item in &app.timeline {
        match item {
            TimelineItem::User(text) => push_section(&mut lines, "YOU", text, app.theme.user),
            TimelineItem::Assistant { text, open } => push_section(
                &mut lines,
                if *open { "ASSISTANT ..." } else { "ASSISTANT" },
                text,
                app.theme.accent,
            ),
            TimelineItem::Reasoning { text, open } => push_section(
                &mut lines,
                if *open { "REASONING ..." } else { "REASONING" },
                text,
                app.theme.muted,
            ),
            TimelineItem::Tool {
                name,
                arguments,
                status,
                output,
            } => {
                lines.push(Line::from(vec![
                    Span::styled(
                        "TOOL ",
                        Style::default()
                            .fg(app.theme.tool)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(display(name)),
                    Span::styled(
                        format!("  [{status}]"),
                        Style::default().fg(app.theme.muted),
                    ),
                ]));
                if !arguments.is_empty() {
                    lines.push(Line::from(Span::styled(
                        truncate(&display(arguments), 180),
                        Style::default().fg(app.theme.muted),
                    )));
                }
                if let Some(output) = output {
                    lines.push(Line::from(Span::styled(
                        truncate(&display(output), 360),
                        Style::default().fg(app.theme.muted),
                    )));
                }
                lines.push(Line::default());
            }
            TimelineItem::Notice(text) => {
                lines.push(Line::from(Span::styled(
                    display(text),
                    Style::default().fg(app.theme.tool),
                )));
                lines.push(Line::default());
            }
            TimelineItem::Error(text) => {
                lines.push(Line::from(Span::styled(
                    display(text),
                    Style::default().fg(app.theme.error),
                )));
                lines.push(Line::default());
            }
        }
    }
    let content_width = usize::from(area.width.saturating_sub(2).max(1));
    let rendered_lines = lines
        .iter()
        .map(|line| line.width().max(1).div_ceil(content_width))
        .sum::<usize>();
    let paragraph = Paragraph::new(Text::from(lines))
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .title(" Conversation ")
                .title_bottom(Line::from(" PgUp/PgDn scroll ").right_aligned())
                .borders(Borders::ALL)
                .border_style(border(app.theme, false)),
        );
    let visible_height = area.height.saturating_sub(2) as usize;
    let bottom = rendered_lines
        .saturating_sub(visible_height)
        .min(usize::from(u16::MAX)) as u16;
    let scroll = bottom.saturating_sub(app.scroll_back);
    frame.render_widget(paragraph.scroll((scroll, 0)), area);
}

fn draw_input(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let title = if app.is_running() {
        " Input: Enter steer / Alt+Enter follow-up "
    } else {
        " Input "
    };
    let view = app
        .input
        .view(usize::from(area.width.saturating_sub(2).max(1)));
    let visible = usize::from(area.height.saturating_sub(2).max(1));
    let top = view.y.saturating_sub(visible - 1);
    let files = format!(
        " {} files / {} images{} | Ctrl+O newline  Ctrl+P/N history  Enter send ",
        app.references.len(),
        app.images.len(),
        if app.reference.is_some() {
            " (capturing)"
        } else {
            ""
        }
    );
    frame.render_widget(
        Paragraph::new(view.lines.join("\n"))
            .scroll((top.min(u16::MAX as usize) as u16, 0))
            .block(
                Block::default()
                    .title(title)
                    .title_bottom(Line::from(files).right_aligned())
                    .borders(Borders::ALL)
                    .border_style(border(app.theme, app.focus == Focus::Input)),
            ),
        area,
    );
    if app.focus == Focus::Input && app.approval.is_none() && !app.confirm_delete {
        frame.set_cursor_position((
            area.x + 1 + view.x as u16,
            area.y + 1 + (view.y - top) as u16,
        ));
    }
}

fn draw_status(frame: &mut Frame<'_>, app: &App, model: &str, area: Rect) {
    let elapsed = app
        .started_at
        .map(|started| started.elapsed())
        .unwrap_or(app.elapsed);
    let status = format!(
        " {} | think {} | {} | round {} | {} tokens | {:.1}s | Esc cancel  Ctrl-C quit ",
        model,
        app.reasoning_effort
            .map_or("default".into(), |value| value.to_string()),
        app.state.name(),
        app.round,
        app.usage
            .total_tokens
            .map(|v| v.to_string())
            .unwrap_or_else(|| "unknown".into()),
        elapsed.as_secs_f32(),
    );
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(display(&status)),
            Line::from(display(&detail(app))),
        ])
        .style(
            Style::default()
                .fg(app.theme.background)
                .bg(app.theme.accent),
        ),
        area,
    );
}

fn detail(app: &App) -> String {
    let known = match (
        app.host_window,
        app.model_spec.as_ref().and_then(|s| s.context_tokens),
    ) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    let context = app
        .model_context
        .as_ref()
        .and_then(|info| info.last_input_count.as_ref())
        .map_or("Context input unknown".into(), |count| {
            format!(
                "Context input {}/{} {:?}/{:?} {}{}",
                count.tokens,
                known.map_or("?".into(), |v| format!(
                    "{} ({:.1}% input)",
                    v,
                    100.0 * count.tokens as f64 / v as f64
                )),
                count.accuracy,
                count.scope,
                count.method,
                if count.fallback.is_some() {
                    " (fallback)"
                } else {
                    ""
                }
            )
        });
    let cost = app
        .cost_estimate()
        .map_or("cost unavailable".into(), |(currency, value)| {
            format!("base est {currency} {value:.3e}")
        });
    format!(" {context} | {cost}")
}

fn draw_approval(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let popup = centered_rect(85, 80, area);
    let approval = app
        .approval
        .as_ref()
        .expect("approval popup without request");
    let body = if !approval.show_arguments
        && let Some(preview) = &approval.invocation.preview
    {
        aicoder_runtime::tools::display_preview(preview)
    } else {
        let arguments =
            serde_json::to_string_pretty(&approval.invocation.arguments).unwrap_or_default();
        let mut end = arguments.len().min(32 * 1024);
        while !arguments.is_char_boundary(end) {
            end -= 1;
        }
        format!(
            "{}{}",
            &arguments[..end],
            if end < arguments.len() {
                "\n[Arguments truncated]"
            } else {
                ""
            }
        )
    };
    let text = if let Some(session) = &approval.session {
        format!(
            "Session {:?} / {:?}\nSession scope: this tool, all arguments\n\n{}",
            session.session_id, session.workspace, body
        )
    } else {
        body
    };
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Block::default().style(
            Style::default()
                .fg(app.theme.foreground)
                .bg(app.theme.background),
        ),
        popup,
    );
    let block = Block::default()
        .title(format!(
            " Approval: {} ",
            display(&approval.invocation.name)
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.tool));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(2)])
        .split(inner);
    frame.render_widget(
        Paragraph::new(display(&text))
            .wrap(Wrap { trim: false })
            .scroll((approval.scroll, 0)),
        parts[0],
    );
    frame.render_widget(
        Paragraph::new(if approval.session.is_some() { "PgUp/PgDn scroll • Tab diff/arguments\ny once • s session/tool (all args) • n deny • Esc cancel" } else { "PgUp/PgDn scroll • Tab diff/arguments\ny allow • n deny • Esc cancel turn" })
            .style(Style::default().fg(app.theme.accent)),
        parts[1],
    );
}

fn draw_delete_confirmation(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let popup = centered_rect(52, 24, area);
    let title = app
        .sessions
        .get(app.selected_session)
        .and_then(|session| session.title.as_deref())
        .unwrap_or("selected session");
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Block::default().style(
            Style::default()
                .fg(app.theme.foreground)
                .bg(app.theme.background),
        ),
        popup,
    );
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(format!("Delete {}?", truncate(title, 48))),
            Line::default(),
            Line::from(Span::styled(
                "y delete permanently   n/Esc keep",
                Style::default()
                    .fg(app.theme.error)
                    .add_modifier(Modifier::BOLD),
            )),
        ])
        .alignment(Alignment::Center)
        .block(
            Block::default()
                .title(" Delete session ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(app.theme.error)),
        ),
        popup,
    );
}

fn push_section<'a>(lines: &mut Vec<Line<'a>>, title: &'a str, text: &'a str, color: Color) {
    lines.push(Line::from(Span::styled(
        title,
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )));
    lines.extend(
        display(text)
            .lines()
            .map(|line| Line::from(line.to_string())),
    );
    lines.push(Line::default());
}

fn border(theme: Theme, active: bool) -> Style {
    Style::default().fg(if active { theme.accent } else { theme.border })
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

fn truncate(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let text = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{text}...")
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use ratatui::{Terminal, backend::TestBackend};

    use super::*;

    #[test]
    fn renders_the_primary_tui_regions() {
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let app = App::new(Vec::new());

        terminal
            .draw(|frame| draw(frame, &app, "test-model"))
            .unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Sessions"));
        assert!(rendered.contains("Conversation"));
        assert!(rendered.contains("Input"));
        assert!(rendered.contains("test-model"));
    }

    #[test]
    fn renders_filtered_slash_command_menu() {
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new(Vec::new());
        app.input.insert('/');

        terminal
            .draw(|frame| draw(frame, &app, "test-model"))
            .unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Commands"));
        assert!(rendered.contains("/exit"));
        assert!(rendered.contains("Exit the TUI"));
    }
    #[test]
    fn approval_diff_can_scroll_without_hiding_decision_controls() {
        use aicoder_core::{
            ApprovalRequestId,
            tools::{ToolCapability, ToolInvocation, ToolPreview},
        };
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new(Vec::new());
        app.approval = Some(crate::app::PendingApproval {
            id: ApprovalRequestId {
                run_id: "test".into(),
                sequence: 1,
            },
            invocation: ToolInvocation {
                call_id: "call".into(),
                name: "edit_file".into(),
                arguments: serde_json::json!({"path":"file"}),
                capability: ToolCapability::Mutating,
                preview: Some(ToolPreview {
                    summary: "Modify file".into(),
                    body: (1..=80).map(|i| format!("+changed line {i}\n")).collect(),
                    format: "unified_diff".into(),
                    truncated: true,
                }),
            },
            scroll: 40,
            show_arguments: false,
            session: None,
        });
        terminal
            .draw(|frame| draw(frame, &app, "test-model"))
            .unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("changed line 40"));
        assert!(rendered.contains("y allow"));
        assert!(rendered.contains("Tab diff/arguments"));
        app.approval.as_mut().unwrap().show_arguments = true;
        app.approval.as_mut().unwrap().scroll = 0;
        terminal
            .draw(|frame| draw(frame, &app, "test-model"))
            .unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("\"path\""));
    }
    #[test]
    fn session_approval_ui_displays_the_scope_and_never_offers_an_unbound_session_grant() {
        use aicoder_core::{
            ApprovalRequestId,
            tools::{ApprovalSessions, ToolCapability, ToolInvocation},
        };
        let root = tempfile::tempdir().unwrap();
        let info = ApprovalSessions::default()
            .session(root.path(), "chat")
            .unwrap()
            .snapshot()
            .session;
        let mut app = App::new(Vec::new());
        app.approval = Some(crate::app::PendingApproval {
            id: ApprovalRequestId {
                run_id: "run".into(),
                sequence: 1,
            },
            invocation: ToolInvocation {
                call_id: "call".into(),
                name: "write_file".into(),
                arguments: serde_json::json!({"path":"file"}),
                capability: ToolCapability::Mutating,
                preview: None,
            },
            scroll: 0,
            show_arguments: false,
            session: Some(info),
        });
        let mut terminal = Terminal::new(TestBackend::new(140, 32)).unwrap();
        terminal.draw(|frame| draw(frame, &app, "offline")).unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("this tool, all arguments"));
        assert!(rendered.contains("s session/tool"));
        assert!(rendered.contains("chat"));
        app.approval.as_mut().unwrap().session = None;
        terminal.draw(|frame| draw(frame, &app, "offline")).unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(!rendered.contains("s session/tool"));
        assert!(rendered.contains("y allow"));
    }
    #[test]
    fn multiline_cursor_and_theme_stay_inside_minimum_terminal_and_controls_are_inert() {
        let mut app = App::new(vec![]);
        app.theme = Theme::named("light").unwrap();
        app.input.insert_text(&"界👩‍💻\n".repeat(20));
        app.timeline
            .push(TimelineItem::Notice("untrusted \x1b[2J".into()));
        let mut terminal = Terminal::new(TestBackend::new(60, 16)).unwrap();
        terminal.draw(|frame| draw(frame, &app, "m1")).unwrap();
        let cursor = terminal.get_cursor_position().unwrap();
        assert!(cursor.x < 60 && cursor.y < 16);
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(!text.contains('\x1b'));
        assert!(text.contains("\\u{1b}"));
    }
}
