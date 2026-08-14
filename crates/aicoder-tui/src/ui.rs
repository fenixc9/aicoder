use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::app::{App, Focus, TimelineItem};

const BORDER: Color = Color::Rgb(76, 86, 106);
const ACCENT: Color = Color::Rgb(136, 192, 208);
const USER: Color = Color::Rgb(163, 190, 140);
const TOOL: Color = Color::Rgb(235, 203, 139);
const ERROR: Color = Color::Rgb(191, 97, 106);
const MUTED: Color = Color::Rgb(129, 161, 193);

pub fn draw(frame: &mut Frame<'_>, app: &App, model: &str) {
    let area = frame.area();
    if area.width < 60 || area.height < 16 {
        frame.render_widget(
            Paragraph::new("Terminal too small\nMinimum: 60 x 16")
                .alignment(Alignment::Center)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(border(false)),
                ),
            area,
        );
        return;
    }

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(8),
            Constraint::Length(5),
            Constraint::Length(1),
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
            Style::default().fg(Color::Black).bg(ACCENT)
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
                .border_style(border(true)),
        ),
        area,
        &mut state,
    );
}

fn draw_sessions(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let items = if app.sessions.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            "No sessions",
            Style::default().fg(MUTED),
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
                        Span::styled(marker, Style::default().fg(ACCENT)),
                        Span::styled(
                            truncate(title, 22),
                            Style::default().add_modifier(if selected {
                                Modifier::BOLD
                            } else {
                                Modifier::empty()
                            }),
                        ),
                    ]),
                    Line::from(Span::styled(
                        format!("  {} messages", session.message_count),
                        Style::default().fg(MUTED),
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
                .border_style(border(app.focus == Focus::Sessions)),
        ),
        area,
    );
}

fn draw_timeline(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let mut lines = Vec::new();
    if app.timeline.is_empty() {
        lines.push(Line::from(Span::styled(
            "Start a conversation from the input below.",
            Style::default().fg(MUTED),
        )));
    }
    for item in &app.timeline {
        match item {
            TimelineItem::User(text) => push_section(&mut lines, "YOU", text, USER),
            TimelineItem::Assistant { text, open } => push_section(
                &mut lines,
                if *open { "ASSISTANT ..." } else { "ASSISTANT" },
                text,
                ACCENT,
            ),
            TimelineItem::Reasoning { text, open } => push_section(
                &mut lines,
                if *open { "REASONING ..." } else { "REASONING" },
                text,
                MUTED,
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
                        Style::default().fg(TOOL).add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(name),
                    Span::styled(format!("  [{status}]"), Style::default().fg(MUTED)),
                ]));
                if !arguments.is_empty() {
                    lines.push(Line::from(Span::styled(
                        truncate(arguments, 180),
                        Style::default().fg(MUTED),
                    )));
                }
                if let Some(output) = output {
                    lines.push(Line::from(Span::styled(
                        truncate(output, 360),
                        Style::default().fg(Color::Gray),
                    )));
                }
                lines.push(Line::default());
            }
            TimelineItem::Notice(text) => {
                lines.push(Line::from(Span::styled(text, Style::default().fg(TOOL))));
                lines.push(Line::default());
            }
            TimelineItem::Error(text) => {
                lines.push(Line::from(Span::styled(text, Style::default().fg(ERROR))));
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
                .border_style(border(false)),
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
        " Input locked while agent runs "
    } else {
        " Input "
    };
    let content_width = area.width.saturating_sub(2).max(1);
    let cursor = u16::try_from(app.input.cursor()).unwrap_or(u16::MAX);
    let horizontal_scroll = cursor.saturating_sub(content_width.saturating_sub(1));
    frame.render_widget(
        Paragraph::new(app.input.value())
            .scroll((0, horizontal_scroll))
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .title(title)
                    .title_bottom(Line::from(" Enter send  Tab focus ").right_aligned())
                    .borders(Borders::ALL)
                    .border_style(border(app.focus == Focus::Input)),
            ),
        area,
    );
    if app.focus == Focus::Input && !app.is_running() && app.approval.is_none() {
        frame.set_cursor_position((
            area.x
                .saturating_add(1 + cursor.saturating_sub(horizontal_scroll)),
            area.y + 1,
        ));
    }
}

fn draw_status(frame: &mut Frame<'_>, app: &App, model: &str, area: Rect) {
    let elapsed = app
        .started_at
        .map(|started| started.elapsed())
        .unwrap_or(app.elapsed);
    let status = format!(
        " {} | {} | round {} | {} tokens | {:.1}s | Esc cancel  Ctrl-C quit ",
        model,
        app.state.name(),
        app.round,
        app.usage.total_tokens,
        elapsed.as_secs_f32(),
    );
    frame.render_widget(
        Paragraph::new(status).style(Style::default().fg(Color::Black).bg(ACCENT)),
        area,
    );
}

fn draw_approval(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let popup = centered_rect(70, 50, area);
    let approval = app
        .approval
        .as_ref()
        .expect("approval popup without request");
    let arguments = serde_json::to_string_pretty(&approval.invocation.arguments)
        .unwrap_or_else(|_| approval.invocation.arguments.to_string());
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled("Tool: ", Style::default().fg(MUTED)),
                Span::styled(
                    &approval.invocation.name,
                    Style::default().fg(TOOL).add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![
                Span::styled("Capability: ", Style::default().fg(MUTED)),
                Span::raw(format!("{:?}", approval.invocation.capability)),
            ]),
            Line::default(),
            Line::from(arguments),
            Line::default(),
            Line::from(Span::styled(
                "y allow   n deny   Esc cancel turn",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            )),
        ])
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .title(" Approval required ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(TOOL)),
        ),
        popup,
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
        Paragraph::new(vec![
            Line::from(format!("Delete {}?", truncate(title, 48))),
            Line::default(),
            Line::from(Span::styled(
                "y delete permanently   n/Esc keep",
                Style::default().fg(ERROR).add_modifier(Modifier::BOLD),
            )),
        ])
        .alignment(Alignment::Center)
        .block(
            Block::default()
                .title(" Delete session ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(ERROR)),
        ),
        popup,
    );
}

fn push_section<'a>(lines: &mut Vec<Line<'a>>, title: &'a str, text: &'a str, color: Color) {
    lines.push(Line::from(Span::styled(
        title,
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )));
    lines.extend(text.lines().map(Line::from));
    lines.push(Line::default());
}

fn border(active: bool) -> Style {
    Style::default().fg(if active { ACCENT } else { BORDER })
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
}
