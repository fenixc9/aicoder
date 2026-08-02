use std::{collections::BTreeMap, sync::Arc};

use anyhow::{Context, Result};

use crate::{
    events::{AgentEvent, AgentEventSink, StreamEnd, ToolCallKey},
    types::{
        ChatCompletionResponse, ChatMessage, Choice, FunctionCall, LogProbs, Role, StreamChunk,
        StreamDelta, StreamToolCall, ToolCall, ToolType, Usage,
    },
};

const MAX_SSE_EVENT_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SseState {
    Open,
    Done,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SseEvent {
    Data(String),
    Done,
}

/// Incrementally decodes SSE lines without assuming HTTP chunks align to events or UTF-8.
pub(crate) struct SseDecoder {
    state: SseState,
    line_buffer: Vec<u8>,
    data: String,
}

impl Default for SseDecoder {
    fn default() -> Self {
        Self {
            state: SseState::Open,
            line_buffer: Vec::new(),
            data: String::new(),
        }
    }
}

impl SseDecoder {
    pub(crate) fn push(&mut self, bytes: &[u8]) -> Result<Vec<SseEvent>> {
        if self.state == SseState::Done {
            return Ok(Vec::new());
        }

        self.line_buffer.extend_from_slice(bytes);
        let mut events = Vec::new();
        let mut consumed = 0_usize;
        while let Some(relative) = self.line_buffer[consumed..]
            .iter()
            .position(|byte| *byte == b'\n')
        {
            let newline = consumed + relative;
            let line = self.line_buffer[consumed..newline].to_vec();
            consumed = newline + 1;
            if let Some(event) = self.process_line(&line)? {
                events.push(event);
            }
        }
        if consumed > 0 {
            self.line_buffer.drain(..consumed);
        }
        if self.line_buffer.len() > MAX_SSE_EVENT_BYTES {
            anyhow::bail!("SSE line exceeds {MAX_SSE_EVENT_BYTES} byte limit");
        }
        Ok(events)
    }

    pub(crate) fn finish(&mut self) -> Result<Vec<SseEvent>> {
        if self.state == SseState::Done {
            self.line_buffer.clear();
            self.data.clear();
            return Ok(Vec::new());
        }

        let mut events = Vec::new();
        if !self.line_buffer.is_empty() {
            let line = std::mem::take(&mut self.line_buffer);
            if let Some(event) = self.process_line(&line)? {
                events.push(event);
            }
        }
        if let Some(event) = self.dispatch_event()? {
            events.push(event);
        }
        Ok(events)
    }

    fn process_line(&mut self, raw_line: &[u8]) -> Result<Option<SseEvent>> {
        if self.state == SseState::Done {
            return Ok(None);
        }
        let raw_line = raw_line.strip_suffix(b"\r").unwrap_or(raw_line);
        if raw_line.is_empty() {
            return self.dispatch_event();
        }
        if raw_line.starts_with(b":") {
            return Ok(None);
        }

        let (field, value) = match raw_line.iter().position(|byte| *byte == b':') {
            Some(index) => (&raw_line[..index], &raw_line[index + 1..]),
            None => (raw_line, &[][..]),
        };
        if field != b"data" {
            return Ok(None);
        }
        let value = value.strip_prefix(b" ").unwrap_or(value);
        let value = std::str::from_utf8(value).context("SSE data is not valid UTF-8")?;
        if !self.data.is_empty() {
            self.data.push('\n');
        }
        self.data.push_str(value);
        if self.data.len() > MAX_SSE_EVENT_BYTES {
            anyhow::bail!("SSE event exceeds {MAX_SSE_EVENT_BYTES} byte limit");
        }
        Ok(None)
    }

    fn dispatch_event(&mut self) -> Result<Option<SseEvent>> {
        if self.data.is_empty() {
            return Ok(None);
        }
        let data = std::mem::take(&mut self.data);
        if data.trim() == "[DONE]" {
            self.state = SseState::Done;
            Ok(Some(SseEvent::Done))
        } else {
            Ok(Some(SseEvent::Data(data)))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccumulatorState {
    Waiting,
    Streaming,
    Finished,
}

#[derive(Default)]
struct ToolCallAccumulator {
    id: String,
    tool_type: Option<ToolType>,
    name: String,
    arguments: String,
}

#[derive(Default)]
struct ChoiceAccumulator {
    role: Option<Role>,
    content: String,
    saw_content: bool,
    reasoning: String,
    saw_reasoning: bool,
    reasoning_event_open: bool,
    content_event_open: bool,
    tool_calls: BTreeMap<i32, ToolCallAccumulator>,
    finish_reason: Option<crate::types::FinishReason>,
    events_ended: bool,
    logprobs: Option<LogProbs>,
}

/// Aggregates streaming deltas into the same response shape used by the agent loop.
pub(crate) struct ChatStreamAccumulator {
    state: AccumulatorState,
    id: Option<String>,
    created: Option<i64>,
    model: Option<String>,
    model_service: Option<String>,
    choices: BTreeMap<i32, ChoiceAccumulator>,
    usage: Option<Usage>,
}

impl Default for ChatStreamAccumulator {
    fn default() -> Self {
        Self {
            state: AccumulatorState::Waiting,
            id: None,
            created: None,
            model: None,
            model_service: None,
            choices: BTreeMap::new(),
            usage: None,
        }
    }
}

impl ChatStreamAccumulator {
    pub(crate) fn push(&mut self, chunk: StreamChunk) -> Result<()> {
        self.push_inner(chunk, None)
    }

    pub(crate) fn push_with_events(
        &mut self,
        chunk: StreamChunk,
        events: &AgentEventSink,
    ) -> Result<()> {
        self.push_inner(chunk, Some(events))
    }

    fn push_inner(&mut self, chunk: StreamChunk, events: Option<&AgentEventSink>) -> Result<()> {
        if self.state == AccumulatorState::Finished {
            anyhow::bail!("Received a stream chunk after the stream was finished");
        }
        self.state = AccumulatorState::Streaming;
        merge_stable(&mut self.id, chunk.id, "id")?;
        // Some OpenAI-compatible providers stamp every SSE chunk separately. The timestamp is
        // descriptive metadata, so retain the first value instead of rejecting an otherwise valid
        // stream when later chunks use a different `created` value.
        self.created.get_or_insert(chunk.created);
        merge_stable(&mut self.model, chunk.model, "model")?;
        if let Some(service) = chunk.service_tier.or(chunk.system) {
            merge_stable(&mut self.model_service, service, "model service")?;
        }
        let chunk_usage = chunk.usage;

        for choice in chunk.choices {
            if choice.index < 0 {
                anyhow::bail!("Stream choice index must be non-negative");
            }
            let accumulated = self.choices.entry(choice.index).or_default();
            if accumulated.finish_reason.is_some() {
                anyhow::bail!("Received delta for finished choice {}", choice.index);
            }
            apply_delta(accumulated, choice.index, choice.delta, events)?;
            if let Some(reason) = choice.finish_reason {
                accumulated.finish_reason = Some(reason);
                if let Some(events) = events {
                    emit_choice_ended(events, choice.index, accumulated)?;
                    accumulated.events_ended = true;
                }
            }
            if choice.logprobs.is_some() {
                accumulated.logprobs = choice.logprobs;
            }
        }
        if let Some(usage) = chunk_usage {
            if let Some(events) = events {
                events.emit(AgentEvent::UsageUpdated {
                    usage: usage.clone().into(),
                });
            }
            self.usage = Some(usage);
        }
        Ok(())
    }

    pub(crate) fn abort_events(&self, events: &AgentEventSink, reason: impl Into<String>) {
        let reason = Arc::<str>::from(reason.into());
        for (choice_index, choice) in &self.choices {
            if choice.events_ended {
                continue;
            }
            let outcome = || StreamEnd::Aborted {
                reason: reason.clone(),
            };
            if choice.reasoning_event_open {
                events.emit(AgentEvent::ReasoningEnded {
                    choice_index: *choice_index,
                    outcome: outcome(),
                });
            }
            if choice.content_event_open {
                events.emit(AgentEvent::ContentEnded {
                    choice_index: *choice_index,
                    outcome: outcome(),
                });
            }
            for tool_index in choice.tool_calls.keys() {
                events.emit(AgentEvent::ToolCallEnded {
                    key: ToolCallKey {
                        choice_index: *choice_index,
                        tool_index: *tool_index,
                    },
                    outcome: outcome(),
                    tool_call: None,
                });
            }
        }
    }

    pub(crate) fn finish(mut self) -> Result<ChatCompletionResponse> {
        self.validate_complete()?;
        self.state = AccumulatorState::Finished;

        let choices = self
            .choices
            .into_iter()
            .map(|(index, choice)| build_choice(index, choice))
            .collect::<Result<Vec<_>>>()?;
        Ok(ChatCompletionResponse {
            id: self.id.context("Streaming response is missing id")?,
            object_type: "chat.completion".to_string(),
            created: self
                .created
                .context("Streaming response is missing created timestamp")?,
            model: self.model.context("Streaming response is missing model")?,
            model_service: self.model_service,
            choices,
            logprobs: None,
            usage: self.usage,
        })
    }

    pub(crate) fn finish_with_events(
        self,
        events: &AgentEventSink,
    ) -> Result<ChatCompletionResponse> {
        if let Err(error) = self.validate_complete() {
            self.abort_events(events, error.to_string());
            return Err(error);
        }
        self.finish()
    }

    fn validate_complete(&self) -> Result<()> {
        if self.state == AccumulatorState::Waiting {
            anyhow::bail!("Streaming response contained no chunks");
        }
        if self.choices.is_empty() {
            anyhow::bail!("Streaming response contained no choices");
        }
        self.id
            .as_ref()
            .context("Streaming response is missing id")?;
        self.created
            .as_ref()
            .context("Streaming response is missing created timestamp")?;
        self.model
            .as_ref()
            .context("Streaming response is missing model")?;
        for choice in self.choices.values() {
            choice
                .finish_reason
                .as_ref()
                .context("Streaming choice ended without finish_reason")?;
            for tool_call in choice.tool_calls.values() {
                build_tool_call(tool_call)?;
            }
        }
        Ok(())
    }
}

fn apply_delta(
    choice: &mut ChoiceAccumulator,
    choice_index: i32,
    delta: StreamDelta,
    events: Option<&AgentEventSink>,
) -> Result<()> {
    if let Some(role) = delta.role {
        merge_stable(&mut choice.role, role, "choice role")?;
    }
    if let Some(reasoning) = delta.reasoning {
        choice.saw_reasoning = true;
        if !reasoning.is_empty()
            && let Some(events) = events
        {
            if !choice.reasoning_event_open {
                events.emit(AgentEvent::ReasoningStarted { choice_index });
                choice.reasoning_event_open = true;
            }
            events.emit(AgentEvent::ReasoningChunk {
                choice_index,
                delta: reasoning.clone().into(),
            });
        }
        choice.reasoning.push_str(&reasoning);
    }
    if let Some(content) = delta.content {
        choice.saw_content = true;
        if !content.is_empty()
            && let Some(events) = events
        {
            if !choice.content_event_open {
                events.emit(AgentEvent::ContentStarted { choice_index });
                choice.content_event_open = true;
            }
            events.emit(AgentEvent::ContentChunk {
                choice_index,
                delta: content.clone().into(),
            });
        }
        choice.content.push_str(&content);
    }
    if let Some(tool_calls) = delta.tool_calls {
        for tool_call in tool_calls {
            apply_tool_call(choice, choice_index, tool_call, events)?;
        }
    }
    Ok(())
}

fn apply_tool_call(
    choice: &mut ChoiceAccumulator,
    choice_index: i32,
    delta: StreamToolCall,
    events: Option<&AgentEventSink>,
) -> Result<()> {
    if delta.index < 0 {
        anyhow::bail!("Stream tool call index must be non-negative");
    }
    let key = ToolCallKey {
        choice_index,
        tool_index: delta.index,
    };
    let is_new = !choice.tool_calls.contains_key(&delta.index);
    if is_new && let Some(events) = events {
        events.emit(AgentEvent::ToolCallStarted { key });
    }

    let id_delta = delta.id;
    let tool_type = delta.tool_type;
    let (name_delta, arguments_delta) = delta
        .function
        .map(|function| (function.name, function.arguments))
        .unwrap_or_default();
    if let Some(events) = events
        && (id_delta.is_some() || name_delta.is_some() || arguments_delta.is_some())
    {
        events.emit(AgentEvent::ToolCallChunk {
            key,
            id_delta: id_delta.clone().map(Into::into),
            name_delta: name_delta.clone().map(Into::into),
            arguments_delta: arguments_delta.clone().map(Into::into),
        });
    }

    let tool_call = choice.tool_calls.entry(delta.index).or_default();
    if let Some(id) = id_delta {
        merge_fragment(&mut tool_call.id, &id);
    }
    if let Some(tool_type) = tool_type {
        merge_stable(&mut tool_call.tool_type, tool_type, "tool call type")?;
    }
    if let Some(name) = name_delta {
        merge_fragment(&mut tool_call.name, &name);
    }
    if let Some(arguments) = arguments_delta {
        merge_arguments(&mut tool_call.arguments, &arguments);
    }
    Ok(())
}

fn emit_choice_ended(
    events: &AgentEventSink,
    choice_index: i32,
    choice: &ChoiceAccumulator,
) -> Result<()> {
    let completed_tools = choice
        .tool_calls
        .iter()
        .map(|(tool_index, tool_call)| {
            Ok((
                ToolCallKey {
                    choice_index,
                    tool_index: *tool_index,
                },
                build_tool_call(tool_call)?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;

    if choice.reasoning_event_open {
        events.emit(AgentEvent::ReasoningEnded {
            choice_index,
            outcome: StreamEnd::Completed,
        });
    }
    if choice.content_event_open {
        events.emit(AgentEvent::ContentEnded {
            choice_index,
            outcome: StreamEnd::Completed,
        });
    }
    for (key, tool_call) in completed_tools {
        events.emit(AgentEvent::ToolCallEnded {
            key,
            outcome: StreamEnd::Completed,
            tool_call: Some(tool_call.into()),
        });
    }
    Ok(())
}

fn merge_fragment(target: &mut String, fragment: &str) {
    if fragment.is_empty() || fragment == target || target.ends_with(fragment) {
        return;
    }
    if fragment.starts_with(target.as_str()) {
        *target = fragment.to_string();
    } else {
        target.push_str(fragment);
    }
}

fn merge_arguments(target: &mut String, fragment: &str) {
    if fragment.is_empty() || fragment == target {
        return;
    }
    if fragment.starts_with(target.as_str()) {
        *target = fragment.to_string();
    } else {
        target.push_str(fragment);
    }
}

fn merge_stable<T>(target: &mut Option<T>, incoming: T, field: &str) -> Result<()>
where
    T: PartialEq,
{
    match target {
        Some(current) if current != &incoming => anyhow::bail!("Streaming {field} changed"),
        Some(_) => {}
        None => *target = Some(incoming),
    }
    Ok(())
}

fn build_choice(index: i32, choice: ChoiceAccumulator) -> Result<Choice> {
    let finish_reason = choice
        .finish_reason
        .context("Streaming choice ended without finish_reason")?;
    let tool_calls = choice
        .tool_calls
        .into_values()
        .map(|tool_call| build_tool_call(&tool_call))
        .collect::<Result<Vec<_>>>()?;
    Ok(Choice {
        index,
        message: ChatMessage {
            role: choice.role.unwrap_or(Role::Assistant),
            content: choice.saw_content.then_some(choice.content),
            reasoning: choice.saw_reasoning.then_some(choice.reasoning),
            tool_calls: (!tool_calls.is_empty()).then_some(tool_calls),
            tool_call_id: None,
            name: None,
        },
        finish_reason: Some(finish_reason),
        logprobs: choice.logprobs,
    })
}

fn build_tool_call(tool_call: &ToolCallAccumulator) -> Result<ToolCall> {
    if tool_call.id.is_empty() {
        anyhow::bail!("Streaming tool call is missing id");
    }
    if tool_call.name.is_empty() {
        anyhow::bail!(
            "Streaming tool call {} is missing function name",
            tool_call.id
        );
    }
    Ok(ToolCall {
        id: tool_call.id.clone(),
        tool_type: tool_call.tool_type.clone().unwrap_or(ToolType::Function),
        function: FunctionCall {
            name: tool_call.name.clone(),
            arguments: tool_call.arguments.clone(),
        },
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::json;

    use super::*;
    use crate::events::AgentEventEnvelope;

    fn chunk(value: serde_json::Value) -> StreamChunk {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn sse_decoder_handles_arbitrary_boundaries_and_multiple_events() {
        let input = concat!(
            ": keepalive\r\n",
            "data: {\"value\":\"你好\"}\r\n\r\n",
            "data: {\"value\":2}\n\n",
            "data: [DONE]\n\n"
        )
        .as_bytes();
        let mut decoder = SseDecoder::default();
        let mut events = Vec::new();
        for byte in input {
            events.extend(decoder.push(std::slice::from_ref(byte)).unwrap());
        }
        events.extend(decoder.finish().unwrap());

        assert_eq!(
            events,
            vec![
                SseEvent::Data("{\"value\":\"你好\"}".to_string()),
                SseEvent::Data("{\"value\":2}".to_string()),
                SseEvent::Done,
            ]
        );
    }

    #[test]
    fn sse_decoder_joins_multiline_data_and_flushes_eof() {
        let mut decoder = SseDecoder::default();
        assert!(decoder.push(b"data: one\ndata: two").unwrap().is_empty());
        assert_eq!(
            decoder.finish().unwrap(),
            vec![SseEvent::Data("one\ntwo".to_string())]
        );
    }

    #[test]
    fn accumulator_builds_content_reasoning_tools_and_usage() {
        let mut accumulator = ChatStreamAccumulator::default();
        accumulator
            .push(chunk(json!({
                "id":"stream-1","object":"chat.completion.chunk","created":1,
                "model":"deepseek-chat","choices":[{"index":0,"delta":{
                    "role":"assistant","reasoning_content":"think ","content":"hello ",
                    "tool_calls":[{"index":0,"id":"call_","type":"function",
                        "function":{"name":"read_","arguments":"{\"pa"}}]
                },"finish_reason":null}]
            })))
            .unwrap();
        accumulator
            .push(chunk(json!({
                "id":"stream-1","object":"chat.completion.chunk","created":1,
                "model":"deepseek-chat","choices":[{"index":0,"delta":{
                    "reasoning_content":"done","content":"world",
                    "tool_calls":[{"index":0,"id":"1",
                        "function":{"name":"file","arguments":"th\":\"a.rs\"}"}}]
                },"finish_reason":"tool_calls"}]
            })))
            .unwrap();
        accumulator
            .push(chunk(json!({
                "id":"stream-1","object":"chat.completion.chunk","created":1,
                "model":"deepseek-chat","choices":[],
                "usage":{"prompt_tokens":10,"completion_tokens":3,"total_tokens":13}
            })))
            .unwrap();

        let response = accumulator.finish().unwrap();
        let message = &response.choices[0].message;
        assert_eq!(message.content.as_deref(), Some("hello world"));
        assert_eq!(message.reasoning.as_deref(), Some("think done"));
        let call = &message.tool_calls.as_ref().unwrap()[0];
        assert_eq!(call.id, "call_1");
        assert_eq!(call.function.name, "read_file");
        assert_eq!(call.function.arguments, "{\"path\":\"a.rs\"}");
        assert_eq!(response.usage.unwrap().total_tokens, 13);
    }

    #[tokio::test]
    async fn accumulator_emits_ordered_semantic_events() {
        let delivered = Arc::new(Mutex::new(Vec::<AgentEventEnvelope>::new()));
        let captured = Arc::clone(&delivered);
        let events = AgentEventSink::new(Arc::new(move |event: &AgentEventEnvelope| {
            captured.lock().unwrap().push(event.clone());
        }));
        let mut accumulator = ChatStreamAccumulator::default();
        accumulator
            .push_with_events(
                chunk(json!({
                    "id":"stream-events","object":"chat.completion.chunk","created":1,
                    "model":"m","choices":[{"index":0,"delta":{
                        "role":"assistant","reasoning_content":"think"
                    },"finish_reason":null}]
                })),
                &events,
            )
            .unwrap();
        accumulator
            .push_with_events(
                chunk(json!({
                    "id":"stream-events","object":"chat.completion.chunk","created":1,
                    "model":"m","choices":[{"index":0,"delta":{
                        "content":"answer","tool_calls":[{"index":0,"id":"call-1",
                            "type":"function","function":{"name":"read_file",
                            "arguments":"{\"path\":\"a.rs\"}"}}]
                    },"finish_reason":"tool_calls"}]
                })),
                &events,
            )
            .unwrap();
        accumulator
            .push_with_events(
                chunk(json!({
                    "id":"stream-events","object":"chat.completion.chunk","created":1,
                    "model":"m","choices":[],
                    "usage":{"prompt_tokens":2,"completion_tokens":3,"total_tokens":5}
                })),
                &events,
            )
            .unwrap();

        let response = accumulator.finish_with_events(&events).unwrap();
        events.shutdown().await;
        assert_eq!(response.usage.unwrap().total_tokens, 5);

        let delivered = delivered.lock().unwrap();
        assert!(matches!(
            delivered[0].event,
            AgentEvent::ReasoningStarted { .. }
        ));
        assert!(matches!(
            delivered[1].event,
            AgentEvent::ReasoningChunk { .. }
        ));
        assert!(matches!(
            delivered[2].event,
            AgentEvent::ContentStarted { .. }
        ));
        assert!(matches!(
            delivered[3].event,
            AgentEvent::ContentChunk { .. }
        ));
        assert!(matches!(
            delivered[4].event,
            AgentEvent::ToolCallStarted { .. }
        ));
        assert!(matches!(
            delivered[5].event,
            AgentEvent::ToolCallChunk { .. }
        ));
        assert!(matches!(
            delivered[6].event,
            AgentEvent::ReasoningEnded { .. }
        ));
        assert!(matches!(
            delivered[7].event,
            AgentEvent::ContentEnded { .. }
        ));
        assert!(matches!(
            delivered[8].event,
            AgentEvent::ToolCallEnded { .. }
        ));
        assert!(matches!(
            delivered[9].event,
            AgentEvent::UsageUpdated { .. }
        ));
        assert!(
            delivered
                .iter()
                .enumerate()
                .all(|(index, event)| event.sequence == index as u64 + 1)
        );
    }

    #[tokio::test]
    async fn accumulator_closes_open_events_when_stream_aborts() {
        let delivered = Arc::new(Mutex::new(Vec::<AgentEventEnvelope>::new()));
        let captured = Arc::clone(&delivered);
        let events = AgentEventSink::new(Arc::new(move |event: &AgentEventEnvelope| {
            captured.lock().unwrap().push(event.clone());
        }));
        let mut accumulator = ChatStreamAccumulator::default();
        accumulator
            .push_with_events(
                chunk(json!({
                    "id":"stream-abort","object":"chat.completion.chunk","created":1,
                    "model":"m","choices":[{"index":0,"delta":{
                        "reasoning_content":"partial","content":"answer",
                        "tool_calls":[{"index":0,"id":"call-1","type":"function",
                            "function":{"name":"read_file","arguments":"{\"path\""}}]
                    },"finish_reason":null}]
                })),
                &events,
            )
            .unwrap();
        accumulator.abort_events(&events, "connection reset");
        events.shutdown().await;

        let delivered = delivered.lock().unwrap();
        let endings = delivered
            .iter()
            .filter(|event| {
                matches!(
                    event.event,
                    AgentEvent::ReasoningEnded { .. }
                        | AgentEvent::ContentEnded { .. }
                        | AgentEvent::ToolCallEnded { .. }
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(endings.len(), 3);
        assert!(endings.iter().all(|event| match &event.event {
            AgentEvent::ReasoningEnded { outcome, .. }
            | AgentEvent::ContentEnded { outcome, .. }
            | AgentEvent::ToolCallEnded { outcome, .. } => matches!(
                outcome,
                StreamEnd::Aborted { reason } if reason.as_ref() == "connection reset"
            ),
            _ => false,
        }));
    }

    #[test]
    fn accumulator_rejects_changed_metadata_and_incomplete_tools() {
        let mut changed = ChatStreamAccumulator::default();
        changed
            .push(chunk(json!({
                "id":"one","object":"chat.completion.chunk","created":1,"model":"m",
                "choices":[{"index":0,"delta":{"role":"assistant"},"finish_reason":null}]
            })))
            .unwrap();
        let error = changed
            .push(chunk(json!({
                "id":"two","object":"chat.completion.chunk","created":1,"model":"m",
                "choices":[]
            })))
            .unwrap_err();
        assert!(error.to_string().contains("id changed"));

        let mut incomplete = ChatStreamAccumulator::default();
        incomplete
            .push(chunk(json!({
                "id":"one","object":"chat.completion.chunk","created":1,"model":"m",
                "choices":[{"index":0,"delta":{"tool_calls":[{"index":0,
                    "function":{"arguments":"{}"}}]},"finish_reason":"tool_calls"}]
            })))
            .unwrap();
        assert!(
            incomplete
                .finish()
                .unwrap_err()
                .to_string()
                .contains("missing id")
        );
    }

    #[test]
    fn accumulator_keeps_first_created_timestamp_when_chunks_change_it() {
        let mut accumulator = ChatStreamAccumulator::default();
        accumulator
            .push(chunk(json!({
                "id":"one","object":"chat.completion.chunk","created":1,"model":"m",
                "choices":[{"index":0,"delta":{"role":"assistant","content":"a"},
                    "finish_reason":null}]
            })))
            .unwrap();
        accumulator
            .push(chunk(json!({
                "id":"one","object":"chat.completion.chunk","created":2,"model":"m",
                "choices":[{"index":0,"delta":{"content":"b"},"finish_reason":"stop"}]
            })))
            .unwrap();

        let response = accumulator.finish().unwrap();
        assert_eq!(response.created, 1);
        assert_eq!(response.choices[0].message.content.as_deref(), Some("ab"));
    }
}
