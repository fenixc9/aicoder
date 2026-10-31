use crate::{editor::InputBuffer, theme::Theme};
use aicoder_core::{TurnExecutionContext, ui::*};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
};
pub struct ExtensionUi {
    pub host: UiHost,
    pub snapshot: UiSnapshot,
    question: Option<UiQuestion>,
    selected: usize,
    draft: InputBuffer,
    last_notice: u64,
    omitted: u64,
}
impl ExtensionUi {
    pub fn new() -> Self {
        let host = UiHost::interactive();
        let snapshot = host.snapshot();
        Self {
            host,
            snapshot,
            question: None,
            selected: 0,
            draft: InputBuffer::default(),
            last_notice: 0,
            omitted: 0,
        }
    }
    pub fn context(&self, context: TurnExecutionContext) -> TurnExecutionContext {
        context.with_ui(self.host.client("extension").expect("fixed owner"))
    }
    pub fn refresh(&mut self) -> Vec<String> {
        self.snapshot = self.host.snapshot();
        let next = self.snapshot.questions.first().cloned();
        if next.as_ref().map(|q| q.id) != self.question.as_ref().map(|q| q.id) {
            self.draft = InputBuffer::default();
            self.selected = 0;
            if let Some(UiQuestion {
                prompt: UiPrompt::Input { initial, .. },
                ..
            }) = &next
            {
                self.draft.replace(initial.clone());
            }
            self.question = next;
        }
        let mut notices = Vec::new();
        if self.snapshot.omitted_notices > self.omitted {
            notices.push("Earlier extension notices are no longer retained.".into());
            self.omitted = self.snapshot.omitted_notices;
        }
        for notice in &self.snapshot.notices {
            if notice.sequence > self.last_notice {
                notices.push(format!(
                    "[{} · {:?}] {}",
                    notice.owner, notice.level, notice.text
                ));
                self.last_notice = notice.sequence;
            }
        }
        notices
    }
    pub fn asking(&self) -> bool {
        self.question.is_some()
    }
    pub fn paste(&mut self, text: &str) -> bool {
        let Some(UiQuestion {
            prompt:
                UiPrompt::Input {
                    max_bytes,
                    multiline,
                    ..
                },
            ..
        }) = &self.question
        else {
            return false;
        };
        if self.draft.value().len().saturating_add(text.len()) > *max_bytes
            || (!*multiline && text.contains(['\r', '\n']))
        {
            return false;
        }
        self.draft.insert_text(text)
    }
    pub fn key(&mut self, key: KeyEvent, width: usize) -> Result<bool, UiError> {
        let Some(question) = self.question.clone() else {
            return Ok(false);
        };
        let mut answer = None;
        if key.code == KeyCode::Esc {
            answer = Some(UiAnswer::Dismissed);
        } else {
            match &question.prompt {
                UiPrompt::Select { options, .. } => match key.code {
                    KeyCode::Up => {
                        self.selected = self.selected.checked_sub(1).unwrap_or(options.len() - 1)
                    }
                    KeyCode::Down => self.selected = (self.selected + 1) % options.len(),
                    KeyCode::Enter => {
                        answer = Some(UiAnswer::Choice {
                            id: options[self.selected].id.clone(),
                        })
                    }
                    _ => {}
                },
                UiPrompt::Input {
                    multiline,
                    max_bytes,
                    ..
                } => match key.code {
                    KeyCode::Enter if *multiline && key.modifiers.contains(KeyModifiers::ALT) => {
                        self.paste("\n");
                    }
                    KeyCode::Enter => {
                        answer = Some(UiAnswer::Text {
                            text: self.draft.value().to_string(),
                        })
                    }
                    KeyCode::Char(c)
                        if !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                    {
                        if self.draft.value().len() + c.len_utf8() <= *max_bytes {
                            self.draft.insert(c);
                        }
                    }
                    KeyCode::Backspace => {
                        self.draft.backspace();
                    }
                    KeyCode::Delete => {
                        self.draft.delete();
                    }
                    KeyCode::Up if *multiline => {
                        self.draft.vertical(width.max(1), false);
                    }
                    KeyCode::Down if *multiline => {
                        self.draft.vertical(width.max(1), true);
                    }
                    KeyCode::Home => self.draft.move_home(),
                    KeyCode::End => self.draft.move_end(),
                    KeyCode::Left => {
                        self.draft.move_left();
                    }
                    KeyCode::Right => {
                        self.draft.move_right();
                    }
                    _ => {}
                },
            }
        }
        if let Some(answer) = answer {
            self.host.answer(question.id, answer)?;
            self.refresh();
        }
        Ok(true)
    }
    pub fn status_line(&self) -> String {
        self.snapshot
            .statuses
            .iter()
            .take(3)
            .map(|s| format!("{}: {}", s.owner, s.text.replace('\n', " ")))
            .collect::<Vec<_>>()
            .join(" · ")
    }
    pub fn draw(&self, frame: &mut Frame<'_>, area: Rect, theme: Theme) {
        let Some(question) = &self.question else {
            return;
        };
        let area = Rect {
            x: area.x + area.width / 10,
            y: area.y + area.height / 6,
            width: area.width * 8 / 10,
            height: area.height * 2 / 3,
        };
        frame.render_widget(Clear, area);
        let style = Style::default().fg(theme.foreground).bg(theme.background);
        let title = match &question.prompt {
            UiPrompt::Select { title, .. } | UiPrompt::Input { title, .. } => {
                crate::editor::display(&title.replace('\n', " "))
            }
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .style(style)
            .title(format!(" {} · {} ", question.owner, title));
        match &question.prompt {
            UiPrompt::Select { options, .. } => {
                let items = options.iter().map(|option| {
                    ListItem::new(crate::editor::display(&option.label.replace('\n', " ")))
                });
                let mut state = ListState::default().with_selected(Some(self.selected));
                frame.render_stateful_widget(
                    List::new(items)
                        .block(block.title_bottom(" ↑/↓ choose · Enter select · Esc dismiss "))
                        .highlight_symbol("> ")
                        .highlight_style(Style::default().fg(theme.accent)),
                    area,
                    &mut state,
                );
            }
            UiPrompt::Input {
                multiline,
                max_bytes,
                ..
            } => {
                let width = usize::from(area.width.saturating_sub(2)).max(1);
                let height = usize::from(area.height.saturating_sub(2)).max(1);
                let view = self.draft.view(width);
                let offset = view.y.saturating_sub(height - 1);
                let help = format!(
                    " {} / {} bytes · Enter submit{} · Esc dismiss ",
                    self.draft.value().len(),
                    max_bytes,
                    if *multiline {
                        " · Alt+Enter newline"
                    } else {
                        ""
                    }
                );
                frame.render_widget(
                    Paragraph::new(view.lines.join("\n"))
                        .style(style)
                        .scroll((offset.min(u16::MAX as usize) as u16, 0))
                        .block(block.title_bottom(help)),
                    area,
                );
                frame.set_cursor_position((
                    area.x + 1 + view.x.min(width - 1) as u16,
                    area.y + 1 + view.y.saturating_sub(offset).min(height - 1) as u16,
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn ready(ui: &mut ExtensionUi) {
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !ui.asking() {
                ui.refresh();
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn choices_are_explicit_keyed_answers() {
        let mut ui = ExtensionUi::new();
        let client = ui.host.client("fixture").unwrap();
        let worker = tokio::spawn(async move {
            client
                .ask(UiPrompt::Select {
                    title: "Pick".into(),
                    options: vec![
                        UiOption {
                            id: "a".into(),
                            label: "A".into(),
                        },
                        UiOption {
                            id: "b".into(),
                            label: "B".into(),
                        },
                    ],
                })
                .await
        });
        ready(&mut ui).await;
        ui.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), 60)
            .unwrap();
        ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 60)
            .unwrap();
        assert_eq!(
            worker.await.unwrap().unwrap(),
            UiAnswer::Choice { id: "b".into() }
        );
    }
    #[tokio::test]
    async fn paste_is_bounded_modal_draft_and_never_submits() {
        let mut ui = ExtensionUi::new();
        let client = ui.host.client("fixture").unwrap();
        let worker = tokio::spawn(async move {
            client
                .ask(UiPrompt::Input {
                    title: "Note".into(),
                    initial: String::new(),
                    multiline: true,
                    max_bytes: 16,
                })
                .await
        });
        ready(&mut ui).await;
        assert!(ui.paste("你好\nline"));
        assert!(!ui.paste("too long for this field"));
        assert!(!worker.is_finished());
        assert_eq!(ui.host.snapshot().questions.len(), 1);
        ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 60)
            .unwrap();
        assert_eq!(
            worker.await.unwrap().unwrap(),
            UiAnswer::Text {
                text: "你好\nline".into()
            }
        );
    }
    #[tokio::test]
    async fn closing_a_scope_expires_modal_state_and_clears_statuses() {
        let mut ui = ExtensionUi::new();
        let client = ui.host.client("fixture").unwrap();
        client.status("work", Some("Waiting".into())).unwrap();
        let worker = tokio::spawn(async move {
            client
                .ask(UiPrompt::Input {
                    title: "Note".into(),
                    initial: String::new(),
                    multiline: false,
                    max_bytes: 16,
                })
                .await
        });
        ready(&mut ui).await;
        let id = ui.host.snapshot().questions[0].id;
        ui.host.close();
        ui.refresh();
        assert!(!ui.asking());
        assert!(ui.snapshot.statuses.is_empty());
        assert!(ui.host.answer(id, UiAnswer::Dismissed).is_err());
        assert_eq!(worker.await.unwrap(), Err(UiError::Closed));
    }
    #[tokio::test]
    async fn dropping_the_tui_owner_cancels_pending_data_without_granting_authority() {
        let mut app = crate::app::App::new(Vec::new());
        let context = app.ui_context(TurnExecutionContext::new());
        let client = context.ui();
        app.begin_command(context.clone());
        let host = app.extension_ui.as_ref().unwrap().host.clone();
        let worker = tokio::spawn(async move {
            client
                .ask(UiPrompt::Input {
                    title: "Note".into(),
                    initial: String::new(),
                    multiline: false,
                    max_bytes: 16,
                })
                .await
        });
        ready(app.extension_ui.as_mut().unwrap()).await;
        drop(app);
        assert!(context.is_cancelled());
        assert!(host.snapshot().closed);
        assert_eq!(worker.await.unwrap(), Err(UiError::Cancelled));
    }
}
