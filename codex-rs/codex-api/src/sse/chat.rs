//! Chat Completions SSE stream processing
//!
//! Handles the simpler Chat Completions SSE format:
//! - `data: {json}\n\n` lines (no event prefix)
//! - `data: [DONE]\n\n` sentinel
//! - ChatCompletionChunk parsing
//! - ResponseEvent emission
//!
//! Tool calls arrive as incremental argument deltas correlated by `index`,
//! with the function `name`/`id` only present in the first delta. We therefore
//! accumulate per-index state across chunks and synthesize the full item
//! lifecycle the downstream consumer expects: `OutputItemAdded(CustomToolCall)`
//! (which registers the argument diff consumer), `ToolCallInputDelta`s, and a
//! final `OutputItemDone(CustomToolCall)` once the turn terminates.

use crate::chat_response::ChatCompletionChunk;
use crate::chat_response::ChatError;
use crate::chat_response::ChatUsage;
use crate::chat_response::map_chat_error_to_api_error;
use crate::common::ResponseEvent;
use crate::error::ApiError;
use codex_client::ByteStream;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::TokenUsage;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::debug;
use tracing::trace;

const RESPONSE_STREAM_CHANNEL_CAPACITY: usize = 1600;

/// Accumulated state for a single in-flight tool call, keyed by its streaming
/// `index` (see [`chunk_to_events`]).
#[derive(Default)]
struct ToolCallAccum {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
    /// Whether `OutputItemAdded` has already been emitted for this index.
    announced: bool,
    /// Whether `OutputItemDone` has already been emitted for this index.
    finalized: bool,
}

pub fn spawn_chat_completions_stream(
    stream_response: codex_client::StreamResponse,
    idle_timeout: Duration,
    telemetry: Option<Arc<dyn crate::telemetry::SseTelemetry>>,
    turn_state: Option<std::sync::Arc<std::sync::OnceLock<String>>>,
) -> crate::common::ResponseStream {
    let upstream_request_id = stream_response
        .headers
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);

    if let Some(turn_state) = turn_state.as_ref()
        && let Some(header_value) = stream_response
            .headers
            .get("x-codex-turn-state")
            .and_then(|v| v.to_str().ok())
    {
        let _ = turn_state.set(header_value.to_string());
    }

    let (tx_event, rx_event) =
        mpsc::channel::<Result<ResponseEvent, ApiError>>(RESPONSE_STREAM_CHANNEL_CAPACITY);
    tokio::spawn(async move {
        process_chat_completions_sse(stream_response.bytes, tx_event, idle_timeout, telemetry)
            .await;
    });

    crate::common::ResponseStream {
        rx_event,
        upstream_request_id,
    }
}

async fn process_chat_completions_sse(
    stream: ByteStream,
    tx_event: mpsc::Sender<Result<ResponseEvent, ApiError>>,
    idle_timeout: Duration,
    telemetry: Option<Arc<dyn crate::telemetry::SseTelemetry>>,
) {
    let mut stream = stream.eventsource();
    let mut tool_state: BTreeMap<usize, ToolCallAccum> = BTreeMap::new();
    let mut response_id = String::new();
    let mut usage: Option<ChatUsage> = None;
    let mut finish_reason: Option<String> = None;
    // Assistant-text accumulation for the synthesized Message item lifecycle.
    let mut text_accum = String::new();
    let mut message_announced = false;

    loop {
        let start = std::time::Instant::now();
        let response = tokio::time::timeout(idle_timeout, stream.next()).await;
        if let Some(t) = telemetry.as_ref() {
            t.on_sse_poll(&response, start.elapsed());
        }
        let sse = match response {
            Ok(Some(Ok(sse))) => sse,
            Ok(Some(Err(e))) => {
                debug!("SSE Error: {e:#}");
                let _ = tx_event.send(Err(ApiError::Stream(e.to_string()))).await;
                return;
            }
            Ok(None) => {
                // Stream closed. If we never saw a terminal signal this is an
                // error (mirrors sse/responses.rs); otherwise synthesize the
                // terminal Completed the consumer requires.
                if finish_reason.is_some() {
                    emit_completion(
                        &tx_event,
                        &response_id,
                        usage.take(),
                        &finish_reason,
                        &mut tool_state,
                        &mut text_accum,
                        message_announced,
                        &response_id,
                    )
                    .await;
                } else {
                    let _ = tx_event
                        .send(Err(ApiError::Stream(
                            "stream closed before completion".into(),
                        )))
                        .await;
                }
                return;
            }
            Err(_) => {
                let _ = tx_event
                    .send(Err(ApiError::Stream("idle timeout waiting for SSE".into())))
                    .await;
                return;
            }
        };

        trace!("SSE data: {}", &sse.data);

        // [DONE] is the provider's explicit terminator: emit the terminal
        // Completed (the consumer breaks its turn loop on this event) and stop.
        if sse.data.trim() == "[DONE]" {
            debug!("Chat Completions stream terminated by [DONE] sentinel");
            emit_completion(
                &tx_event,
                &response_id,
                usage.take(),
                &finish_reason,
                &mut tool_state,
                &mut text_accum,
                message_announced,
                &response_id,
            )
            .await;
            return;
        }

        let chunk = match serde_json::from_str::<ChatCompletionChunk>(&sse.data) {
            Ok(chunk) => chunk,
            Err(_) => {
                // The frame may carry a provider error object rather than a
                // chunk; surface it instead of silently dropping the stream.
                if let Ok(chat_err) = serde_json::from_str::<ChatError>(&sse.data) {
                    let _ = tx_event
                        .send(Err(map_chat_error_to_api_error(chat_err)))
                        .await;
                    return;
                }
                debug!("Failed to parse ChatCompletionChunk, data: {}", &sse.data);
                continue;
            }
        };

        if !chunk.id.is_empty() {
            response_id = chunk.id.clone();
        }
        if chunk.usage.is_some() {
            usage = chunk.usage.clone();
        }

        let (events, fr) = chunk_to_events(
            &chunk,
            &mut tool_state,
            &mut text_accum,
            &mut message_announced,
            &response_id,
        );
        if fr.is_some() {
            finish_reason = fr;
        }
        for event in events {
            let _ = tx_event.send(Ok(event)).await;
        }
    }
}

/// Converts a single [`ChatCompletionChunk`] into zero or more
/// [`ResponseEvent`]s, updating the cross-chunk tool-call accumulator state.
///
/// Returns the events plus the `finish_reason` observed in this chunk (if any);
/// the terminal `Completed` event is emitted separately by the caller so it can
/// carry buffered usage and is guaranteed exactly once.
fn chunk_to_events(
    chunk: &ChatCompletionChunk,
    tool_state: &mut BTreeMap<usize, ToolCallAccum>,
    text_accum: &mut String,
    message_announced: &mut bool,
    message_id: &str,
) -> (Vec<ResponseEvent>, Option<String>) {
    let mut events = Vec::new();
    let mut finish_reason: Option<String> = None;

    for choice in &chunk.choices {
        // Assistant text delta. The downstream turn consumer only accepts
        // OutputTextDelta once an OutputItemAdded(Message) has registered the
        // active item (otherwise it drops/panics on "OutputTextDelta without
        // active item"). The Responses API provides this envelope from the
        // provider; the Chat path receives only content fragments and must
        // synthesize the assistant-Message item lifecycle itself.
        if let Some(content) = &choice.delta.content
            && !content.is_empty()
        {
            if !*message_announced {
                events.push(ResponseEvent::OutputItemAdded(ResponseItem::Message {
                    id: Some(message_id.to_string()),
                    role: "assistant".to_string(),
                    content: Vec::new(),
                    phase: None,
                }));
                *message_announced = true;
            }
            text_accum.push_str(content);
            events.push(ResponseEvent::OutputTextDelta(content.clone()));
        }

        // Tool-call deltas (accumulate across chunks by index).
        if let Some(tool_calls) = &choice.delta.tool_calls {
            for tc in tool_calls {
                let accum = tool_state.entry(tc.index).or_default();
                if let Some(id) = &tc.id {
                    accum.id = Some(id.clone());
                }
                if let Some(func) = &tc.function {
                    if let Some(name) = &func.name {
                        accum.name = Some(name.clone());
                    }
                    if let Some(args) = &func.arguments {
                        accum.arguments.push_str(args);
                    }
                }

                let call_id = accum
                    .id
                    .clone()
                    .unwrap_or_else(|| format!("tool_{}", tc.index));

                // Announce the item once we have an identity. Chat Completions
                // exposes every tool (built-in and MCP) as a function, so the
                // call is surfaced as a FunctionCall — `build_tool_call` maps
                // that to `ToolPayload::Function`, which both built-in tools
                // (exec_command/apply_patch) and MCP tools expect.
                if !accum.announced && (accum.id.is_some() || accum.name.is_some()) {
                    events.push(ResponseEvent::OutputItemAdded(
                        ResponseItem::FunctionCall {
                            id: None,
                            name: accum.name.clone().unwrap_or_default(),
                            namespace: None,
                            arguments: String::new(),
                            call_id: call_id.clone(),
                        },
                    ));
                    accum.announced = true;
                }

                // Forward the incremental argument delta.
                if let Some(args) = tc.function.as_ref().and_then(|f| f.arguments.as_deref())
                    && !args.is_empty()
                {
                    events.push(ResponseEvent::ToolCallInputDelta {
                        item_id: call_id.clone(),
                        call_id: Some(call_id),
                        delta: args.to_string(),
                    });
                }
            }
        }

        // A finish_reason terminates the model's output. Finalize every
        // announced tool call (carrying the fully-assembled arguments) so the
        // consumer queues it for execution.
        if let Some(reason) = &choice.finish_reason {
            finish_reason = Some(reason.clone());
            for (idx, accum) in tool_state.iter_mut() {
                if accum.announced && !accum.finalized {
                    let call_id = accum.id.clone().unwrap_or_else(|| format!("tool_{idx}"));
                    events.push(ResponseEvent::OutputItemDone(
                        ResponseItem::FunctionCall {
                            id: None,
                            name: accum.name.clone().unwrap_or_default(),
                            namespace: None,
                            arguments: accum.arguments.clone(),
                            call_id,
                        },
                    ));
                    accum.finalized = true;
                }
            }
        }
    }

    (events, finish_reason)
}

/// Maps the observed finish_reason to the `end_turn` flag on the terminal
/// `Completed` event. A `tool_calls` finish means the turn is NOT over — core
/// must execute the tools and continue — so `end_turn` is `false`.
fn finish_reason_to_end_turn(finish_reason: Option<&str>) -> Option<bool> {
    match finish_reason {
        Some("tool_calls") => Some(false),
        _ => Some(true),
    }
}

/// Builds the terminal events: a safety-net `OutputItemDone` for any announced
/// tool call that was never finalized (e.g. a provider that omits the
/// `finish_reason` chunk), followed by exactly one `Completed`.
fn build_terminal_events(
    response_id: &str,
    usage: Option<ChatUsage>,
    finish_reason: &Option<String>,
    tool_state: &mut BTreeMap<usize, ToolCallAccum>,
    text_accum: &mut String,
    message_announced: bool,
    message_id: &str,
) -> Vec<ResponseEvent> {
    let mut events = Vec::new();

    // Finalize the assistant message (plain text) before tool calls and before
    // the terminal Completed, carrying the fully-assembled text so the
    // transcript records the complete assistant message.
    if message_announced {
        events.push(ResponseEvent::OutputItemDone(ResponseItem::Message {
            id: Some(message_id.to_string()),
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: std::mem::take(text_accum),
            }],
            phase: None,
        }));
    }

    for (idx, accum) in tool_state.iter_mut() {
        if accum.announced && !accum.finalized {
            let call_id = accum.id.clone().unwrap_or_else(|| format!("tool_{idx}"));
            events.push(ResponseEvent::OutputItemDone(
                ResponseItem::FunctionCall {
                    id: None,
                    name: accum.name.clone().unwrap_or_default(),
                    namespace: None,
                    arguments: accum.arguments.clone(),
                    call_id,
                },
            ));
            accum.finalized = true;
        }
    }

    let token_usage = usage.map(|u| TokenUsage {
        input_tokens: u.prompt_tokens as i64,
        cached_input_tokens: 0,
        output_tokens: u.completion_tokens as i64,
        reasoning_output_tokens: 0,
        total_tokens: u.total_tokens as i64,
    });

    events.push(ResponseEvent::Completed {
        response_id: response_id.to_string(),
        token_usage,
        end_turn: finish_reason_to_end_turn(finish_reason.as_deref()),
    });

    events
}

/// Emits the terminal events (see [`build_terminal_events`]) and drains them to
/// the channel. This guarantees the consumer always observes exactly one
/// `Completed`, regardless of which termination path the stream took.
#[allow(clippy::too_many_arguments)]
async fn emit_completion(
    tx_event: &mpsc::Sender<Result<ResponseEvent, ApiError>>,
    response_id: &str,
    usage: Option<ChatUsage>,
    finish_reason: &Option<String>,
    tool_state: &mut BTreeMap<usize, ToolCallAccum>,
    text_accum: &mut String,
    message_announced: bool,
    message_id: &str,
) {
    let events = build_terminal_events(
        response_id,
        usage,
        finish_reason,
        tool_state,
        text_accum,
        message_announced,
        message_id,
    );
    for event in events {
        let _ = tx_event.send(Ok(event)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat_response::ChatChoiceDelta;
    use crate::chat_response::ChatDelta;
    use crate::chat_response::ChatFunctionCallDelta;
    use crate::chat_response::ChatToolCallDelta;

    fn chunk_with_content(id: &str, content: Option<&str>) -> ChatCompletionChunk {
        ChatCompletionChunk {
            id: id.to_string(),
            object: "chat.completion.chunk".to_string(),
            created: 0,
            model: "gpt-5".to_string(),
            choices: vec![ChatChoiceDelta {
                index: 0,
                delta: ChatDelta {
                    role: None,
                    content: content.map(str::to_string),
                    tool_calls: None,
                },
                finish_reason: None,
            }],
            usage: None,
        }
    }

    // Assistant-text/message accumulator state threaded through chunk_to_events
    // and build_terminal_events.
    #[derive(Default)]
    struct TextState {
        accum: String,
        announced: bool,
    }

    #[test]
    fn test_chunk_text_delta_emits_message_envelope_then_delta() {
        let mut state = BTreeMap::new();
        let mut ts = TextState::default();
        let chunk = chunk_with_content("c1", Some("Hello"));
        let (events, fr) =
            chunk_to_events(&chunk, &mut state, &mut ts.accum, &mut ts.announced, "c1");
        assert!(fr.is_none());
        // OutputItemAdded(Message) envelope first (registers the active item),
        // then the text delta the consumer will stream.
        assert!(matches!(events[0], ResponseEvent::OutputItemAdded(_)));
        assert!(matches!(
            events[1],
            ResponseEvent::OutputTextDelta(ref t) if t == "Hello"
        ));
        assert_eq!(ts.accum, "Hello");
        assert!(ts.announced);
    }

    #[test]
    fn test_multibyte_content_not_corrupted() {
        // Emoji (4-byte) and CJK (3-byte) must survive intact (no byte-splitting).
        let mut state = BTreeMap::new();
        let mut ts = TextState::default();
        let text = "héllo 世界 🦀";
        let chunk = chunk_with_content("c1", Some(text));
        let (events, _) =
            chunk_to_events(&chunk, &mut state, &mut ts.accum, &mut ts.announced, "c1");
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ResponseEvent::OutputTextDelta(t) if t == text))
        );
        assert_eq!(ts.accum, text);
    }

    #[test]
    fn test_text_lifecycle_finalizes_message_with_full_text() {
        // Two text chunks then a terminal finish: the synthesized lifecycle is
        // OutputItemAdded(Message) + OutputTextDelta* (streamed), then on
        // termination OutputItemDone(Message) carrying the full text, then Completed.
        let mut state = BTreeMap::new();
        let mut ts = TextState::default();
        for fragment in ["Hello", " world"] {
            let chunk = chunk_with_content("c1", Some(fragment));
            let _ = chunk_to_events(&chunk, &mut state, &mut ts.accum, &mut ts.announced, "c1");
        }
        assert_eq!(ts.accum, "Hello world");
        let finish = Some("stop".to_string());
        let term = build_terminal_events(
            "c1",
            None,
            &finish,
            &mut state,
            &mut ts.accum,
            ts.announced,
            "c1",
        );
        // OutputItemDone(Message) with the fully-assembled text precedes Completed.
        let done = term.iter().find_map(|e| match e {
            ResponseEvent::OutputItemDone(ResponseItem::Message { content, .. }) => {
                content.first().and_then(|c| match c {
                    ContentItem::OutputText { text } => Some(text.clone()),
                    _ => None,
                })
            }
            _ => None,
        });
        assert_eq!(done.as_deref(), Some("Hello world"));
        // text_accum was consumed by mem::take.
        assert!(ts.accum.is_empty());
        assert!(term.iter().any(|e| matches!(
            e,
            ResponseEvent::Completed {
                end_turn: Some(true),
                ..
            }
        )));
    }

    #[test]
    fn test_tool_call_announces_then_deltas() {
        let mut state = BTreeMap::new();
        let mut ts = TextState::default();
        // First delta: id + name arrive.
        let first = ChatCompletionChunk {
            id: "c1".to_string(),
            object: "chat.completion.chunk".to_string(),
            created: 0,
            model: "gpt-5".to_string(),
            choices: vec![ChatChoiceDelta {
                index: 0,
                delta: ChatDelta {
                    role: None,
                    content: None,
                    tool_calls: Some(vec![ChatToolCallDelta {
                        index: 0,
                        id: Some("call_1".to_string()),
                        r#type: Some("function".to_string()),
                        function: Some(ChatFunctionCallDelta {
                            name: Some("get_weather".to_string()),
                            arguments: Some(r#"{"loc""#.to_string()),
                        }),
                    }]),
                },
                finish_reason: None,
            }],
            usage: None,
        };
        let (events, fr) =
            chunk_to_events(&first, &mut state, &mut ts.accum, &mut ts.announced, "c1");
        assert!(fr.is_none());
        // OutputItemAdded (FunctionCall) + ToolCallInputDelta.
        assert!(events.iter().any(|e| matches!(
            e,
            ResponseEvent::OutputItemAdded(ResponseItem::FunctionCall { name, .. }) if name == "get_weather"
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            ResponseEvent::ToolCallInputDelta { delta, .. } if delta == r#"{"loc""#
        )));

        // Second delta: more argument fragments (name/id absent).
        let second = ChatCompletionChunk {
            id: "c1".to_string(),
            object: "chat.completion.chunk".to_string(),
            created: 0,
            model: "gpt-5".to_string(),
            choices: vec![ChatChoiceDelta {
                index: 0,
                delta: ChatDelta {
                    role: None,
                    content: None,
                    tool_calls: Some(vec![ChatToolCallDelta {
                        index: 0,
                        id: None,
                        r#type: None,
                        function: Some(ChatFunctionCallDelta {
                            name: None,
                            arguments: Some(r#":"SF"}"#.to_string()),
                        }),
                    }]),
                },
                finish_reason: None,
            }],
            usage: None,
        };
        let (events2, _) =
            chunk_to_events(&second, &mut state, &mut ts.accum, &mut ts.announced, "c1");
        // No re-announce; just the delta.
        assert!(
            !events2
                .iter()
                .any(|e| matches!(e, ResponseEvent::OutputItemAdded(_)))
        );
        assert!(events2.iter().any(|e| matches!(
            e,
            ResponseEvent::ToolCallInputDelta { delta, .. } if delta == r#":"SF"}"#
        )));

        // Accumulated arguments preserved in state.
        assert_eq!(state.get(&0).unwrap().arguments, r#"{"loc":"SF"}"#);
    }

    #[test]
    fn test_finish_reason_tool_calls_finalizes_and_signals_continue() {
        let mut state = BTreeMap::new();
        let mut ts = TextState::default();
        // Announce a tool call first.
        let announce = ChatCompletionChunk {
            id: "c1".to_string(),
            object: "chat.completion.chunk".to_string(),
            created: 0,
            model: "gpt-5".to_string(),
            choices: vec![ChatChoiceDelta {
                index: 0,
                delta: ChatDelta {
                    role: None,
                    content: None,
                    tool_calls: Some(vec![ChatToolCallDelta {
                        index: 0,
                        id: Some("call_1".to_string()),
                        r#type: Some("function".to_string()),
                        function: Some(ChatFunctionCallDelta {
                            name: Some("get_weather".to_string()),
                            arguments: Some(r#"{"loc":"SF"}"#.to_string()),
                        }),
                    }]),
                },
                finish_reason: None,
            }],
            usage: None,
        };
        let _ = chunk_to_events(
            &announce,
            &mut state,
            &mut ts.accum,
            &mut ts.announced,
            "c1",
        );

        // Final chunk carries finish_reason = "tool_calls".
        let final_chunk = ChatCompletionChunk {
            id: "c1".to_string(),
            object: "chat.completion.chunk".to_string(),
            created: 0,
            model: "gpt-5".to_string(),
            choices: vec![ChatChoiceDelta {
                index: 0,
                delta: ChatDelta::default(),
                finish_reason: Some("tool_calls".to_string()),
            }],
            usage: None,
        };
        let (events, fr) = chunk_to_events(
            &final_chunk,
            &mut state,
            &mut ts.accum,
            &mut ts.announced,
            "c1",
        );
        assert_eq!(fr.as_deref(), Some("tool_calls"));
        assert!(events.iter().any(|e| matches!(
            e,
            ResponseEvent::OutputItemDone(ResponseItem::FunctionCall { name, arguments, .. })
                if name == "get_weather" && arguments == r#"{"loc":"SF"}"#
        )));

        // Terminal Completed: tool_calls => end_turn false (turn continues).
        let term = build_terminal_events(
            "c1",
            None,
            &fr,
            &mut state,
            &mut ts.accum,
            ts.announced,
            "c1",
        );
        assert!(term.iter().any(|e| matches!(
            e,
            ResponseEvent::Completed {
                end_turn: Some(false),
                ..
            }
        )));
    }

    #[test]
    fn test_multi_element_chunk_not_truncated() {
        // A single chunk carrying content + two parallel tool calls must emit
        // all of them (no early return after the first delta).
        let mut state = BTreeMap::new();
        let mut ts = TextState::default();
        let chunk = ChatCompletionChunk {
            id: "c1".to_string(),
            object: "chat.completion.chunk".to_string(),
            created: 0,
            model: "gpt-5".to_string(),
            choices: vec![ChatChoiceDelta {
                index: 0,
                delta: ChatDelta {
                    role: None,
                    content: Some("thinking".to_string()),
                    tool_calls: Some(vec![
                        ChatToolCallDelta {
                            index: 0,
                            id: Some("call_a".to_string()),
                            r#type: Some("function".to_string()),
                            function: Some(ChatFunctionCallDelta {
                                name: Some("t0".to_string()),
                                arguments: Some("{}".to_string()),
                            }),
                        },
                        ChatToolCallDelta {
                            index: 1,
                            id: Some("call_b".to_string()),
                            r#type: Some("function".to_string()),
                            function: Some(ChatFunctionCallDelta {
                                name: Some("t1".to_string()),
                                arguments: Some("{}".to_string()),
                            }),
                        },
                    ]),
                },
                finish_reason: None,
            }],
            usage: None,
        };
        let (events, _) =
            chunk_to_events(&chunk, &mut state, &mut ts.accum, &mut ts.announced, "c1");
        let added: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                ResponseEvent::OutputItemAdded(ResponseItem::FunctionCall { name, .. }) => {
                    Some(name.clone())
                }
                _ => None,
            })
            .collect();
        assert_eq!(added, vec!["t0".to_string(), "t1".to_string()]);
        // Content delta also present.
        assert!(events.iter().any(|e| matches!(
            e,
            ResponseEvent::OutputTextDelta(t) if t == "thinking"
        )));
    }

    #[test]
    fn test_end_turn_mapping() {
        assert_eq!(finish_reason_to_end_turn(Some("stop")), Some(true));
        assert_eq!(finish_reason_to_end_turn(Some("tool_calls")), Some(false));
        assert_eq!(finish_reason_to_end_turn(Some("length")), Some(true));
        assert_eq!(
            finish_reason_to_end_turn(Some("content_filter")),
            Some(true)
        );
        assert_eq!(finish_reason_to_end_turn(None), Some(true));
    }

    #[test]
    fn test_error_payload_routes_through_mapper() {
        let error_json = r#"{"error":{"message":"rate limited","type":"rate_limit_error","code":"rate_limit_exceeded"}}"#;
        let chat_err: ChatError = serde_json::from_str(error_json).unwrap();
        let api_error = map_chat_error_to_api_error(chat_err);
        assert!(matches!(api_error, ApiError::RateLimit(_)));
    }
}
