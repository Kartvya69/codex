//! End-to-end integration tests for Chat Completions Wire API
//!
//! Tests cover:
//! - Full request/response cycle through client
//! - Backward compatibility with Responses API
//! - Config validation (compaction + Chat = error)
//! - Transport selection (Chat skips WebSocket, goes to SSE)

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use bytes::Bytes;
use codex_api::AuthProvider;
use codex_api::ChatCompletionsOptions;
use codex_api::Compression;
use codex_api::Provider;
use codex_api::ResponseEvent;
use codex_api::ResponsesOptions;
use codex_client::HttpTransport;
use codex_client::Request;
use codex_client::Response;
use codex_client::StreamResponse;
use codex_client::TransportError;
use futures::StreamExt;
use http::HeaderMap;
use http::StatusCode;
use pretty_assertions::assert_eq;
use serde_json::Value;

// ============================================================================
// Test Fixtures
// ============================================================================

#[derive(Clone)]
struct FixtureSseTransport {
    body: String,
}

impl FixtureSseTransport {
    fn new(body: String) -> Self {
        Self { body }
    }
}

impl HttpTransport for FixtureSseTransport {
    async fn execute(&self, _req: Request) -> Result<Response, TransportError> {
        Err(TransportError::Build("execute should not run".to_string()))
    }

    async fn stream(&self, _req: Request) -> Result<StreamResponse, TransportError> {
        let stream =
            futures::stream::iter([Ok::<Bytes, TransportError>(Bytes::from(self.body.clone()))]);
        Ok(StreamResponse {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
            bytes: Box::pin(stream),
        })
    }
}

#[derive(Clone, Default)]
struct NoAuth;

impl AuthProvider for NoAuth {
    fn add_auth_headers(&self, _headers: &mut HeaderMap) {}
}

fn provider(name: &str) -> Provider {
    Provider {
        name: name.to_string(),
        base_url: "https://example.com/v1".to_string(),
        query_params: None,
        headers: HeaderMap::new(),
        retry: codex_api::RetryConfig {
            max_attempts: 1,
            base_delay: Duration::from_millis(1),
            retry_429: false,
            retry_5xx: false,
            retry_transport: true,
        },
        stream_idle_timeout: Duration::from_millis(50),
    }
}

/// Build Chat Completions SSE body from events.
fn build_chat_completions_body(events: Vec<Value>) -> Result<String> {
    let mut body = String::new();
    for e in events {
        let serialized = serde_json::to_string(&e).context("serialize chat completions event")?;
        body.push_str(&format!("data: {serialized}\n\n"));
    }
    // [DONE] sentinel terminates the stream.
    body.push_str("data: [DONE]\n\n");
    Ok(body)
}

// ============================================================================
// Test 1: OpenAI Chat Completions End-to-End
// ============================================================================

#[tokio::test]
async fn openai_chat_completions_e2e() -> Result<()> {
    let chunk1 = serde_json::json!({
        "id": "chatcmpl-123",
        "object": "chat.completion.chunk",
        "created": 1677652288,
        "model": "gpt-5",
        "choices": [{
            "index": 0,
            "delta": { "role": "assistant", "content": "Hello" },
            "finish_reason": null
        }]
    });

    let chunk2 = serde_json::json!({
        "id": "chatcmpl-123",
        "object": "chat.completion.chunk",
        "created": 1677652288,
        "model": "gpt-5",
        "choices": [{
            "index": 0,
            "delta": { "content": " world" },
            "finish_reason": null
        }]
    });

    let chunk3 = serde_json::json!({
        "id": "chatcmpl-123",
        "object": "chat.completion.chunk",
        "created": 1677652288,
        "model": "gpt-5",
        "choices": [{
            "index": 0,
            "delta": {},
            "finish_reason": "stop"
        }]
    });

    let body = build_chat_completions_body(vec![chunk1, chunk2, chunk3])?;
    let transport = FixtureSseTransport::new(body);
    let client =
        codex_api::ChatCompletionsClient::new(transport, provider("openai"), Arc::new(NoAuth));

    let request = serde_json::json!({
        "model": "gpt-5",
        "messages": [{"role": "user", "content": "Hi"}]
    });

    let mut stream = client
        .stream_request(
            request,
            ChatCompletionsOptions {
                compression: Compression::None,
                ..Default::default()
            },
        )
        .await?;

    let mut events = Vec::new();
    while let Some(ev) = stream.next().await {
        if let Ok(ev) = ev {
            events.push(ev);
        }
    }

    // The Chat path must synthesize the assistant-Message item lifecycle that
    // the Responses API provides from the provider: OutputItemAdded(Message)
    // (registers the active item the turn loop needs before text deltas),
    // OutputTextDelta* (streamed text), then OutputItemDone(Message) carrying
    // the fully-assembled text, then the terminal Completed.
    use codex_protocol::models::ContentItem;
    use codex_protocol::models::ResponseItem;

    assert!(events.iter().any(|e| matches!(
        e,
        ResponseEvent::OutputItemAdded(ResponseItem::Message { role, content, .. })
            if role == "assistant" && content.is_empty()
    )));
    let deltas: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            ResponseEvent::OutputTextDelta(t) => Some(t.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(deltas, vec!["Hello".to_string(), " world".to_string()]);
    let final_text = events.iter().find_map(|e| match e {
        ResponseEvent::OutputItemDone(ResponseItem::Message { content, .. }) => {
            content.first().and_then(|c| match c {
                ContentItem::OutputText { text } => Some(text.clone()),
                _ => None,
            })
        }
        _ => None,
    });
    assert_eq!(final_text.as_deref(), Some("Hello world"));
    assert!(events.iter().any(|e| matches!(
        e,
        ResponseEvent::Completed {
            end_turn: Some(true),
            ..
        }
    )));
    Ok(())
}

// ============================================================================
// Test 2: Responses API Still Works
// ============================================================================

#[tokio::test]
async fn responses_api_still_works() -> Result<()> {
    // Verify existing Responses API path is unchanged.
    let item1 = serde_json::json!({
        "type": "response.output_item.done",
        "item": {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": "Test"}]
        }
    });

    let completed = serde_json::json!({
        "type": "response.completed",
        "response": { "id": "resp1" }
    });

    let mut body = String::new();
    for e in [item1, completed] {
        let kind = e
            .get("type")
            .and_then(|v| v.as_str())
            .context("missing type")?;
        let single_key = e.as_object().map(|o| o.len() == 1).unwrap_or(false);
        if single_key {
            body.push_str(&format!("event: {kind}\n\n"));
        } else {
            body.push_str(&format!("event: {kind}\ndata: {e}\n\n"));
        }
    }

    let transport = FixtureSseTransport::new(body);
    let client = codex_api::ResponsesClient::new(transport, provider("claude"), Arc::new(NoAuth));

    let request = codex_api::ResponsesApiRequest {
        model: "gpt-4".to_string(),
        instructions: String::new(),
        input: vec![],
        tools: vec![],
        tool_choice: "auto".to_string(),
        parallel_tool_calls: false,
        reasoning: None,
        store: false,
        stream: true,
        include: vec![],
        service_tier: None,
        prompt_cache_key: None,
        text: None,
        client_metadata: None,
    };

    let mut stream = client
        .stream_request(
            request,
            ResponsesOptions {
                compression: Compression::None,
                ..Default::default()
            },
        )
        .await?;

    let mut events = Vec::new();
    while let Some(ev) = stream.next().await {
        if let Ok(ev) = ev {
            events.push(ev);
        }
    }

    let events: Vec<ResponseEvent> = events
        .into_iter()
        .filter(|ev| !matches!(ev, ResponseEvent::RateLimits(_)))
        .collect();

    assert_eq!(events.len(), 2);
    assert!(matches!(events[0], ResponseEvent::OutputItemDone(_)));
    assert!(matches!(events[1], ResponseEvent::Completed { .. }));
    Ok(())
}

// ============================================================================
// Test 3 & 4: Core-crate concerns (ModelClient construction / WebSocket
// prewarm). These live in core/src/client_tests.rs.
// ============================================================================

// ============================================================================
// Test 5: Tool Calls via Chat Completions
// ============================================================================

#[tokio::test]
async fn chat_completions_tool_calls_e2e() -> Result<()> {
    let chunk1 = serde_json::json!({
        "id": "chatcmpl-456",
        "object": "chat.completion.chunk",
        "created": 1677652289,
        "model": "gpt-5",
        "choices": [{
            "index": 0,
            "delta": { "role": "assistant", "content": null },
            "finish_reason": null
        }]
    });

    let chunk2 = serde_json::json!({
        "id": "chatcmpl-456",
        "object": "chat.completion.chunk",
        "created": 1677652289,
        "model": "gpt-5",
        "choices": [{
            "index": 0,
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": "call_123",
                    "type": "function",
                    "function": { "name": "search", "arguments": "{\"query\":\"test\"}" }
                }]
            },
            "finish_reason": null
        }]
    });

    let chunk3 = serde_json::json!({
        "id": "chatcmpl-456",
        "object": "chat.completion.chunk",
        "created": 1677652289,
        "model": "gpt-5",
        "choices": [{
            "index": 0,
            "delta": {},
            "finish_reason": "tool_calls"
        }]
    });

    let body = build_chat_completions_body(vec![chunk1, chunk2, chunk3])?;
    let transport = FixtureSseTransport::new(body);
    let client =
        codex_api::ChatCompletionsClient::new(transport, provider("openai"), Arc::new(NoAuth));

    let request = serde_json::json!({
        "model": "gpt-5",
        "messages": [{"role": "user", "content": "Search for test"}],
        "tools": [{
            "type": "function",
            "function": {
                "name": "search",
                "description": "Search database",
                "parameters": {
                    "type": "object",
                    "properties": { "query": {"type": "string"} },
                    "required": ["query"]
                }
            }
        }]
    });

    let mut stream = client
        .stream_request(
            request,
            ChatCompletionsOptions {
                compression: Compression::None,
                ..Default::default()
            },
        )
        .await?;

    let mut events = Vec::new();
    while let Some(ev) = stream.next().await {
        if let Ok(ev) = ev {
            events.push(ev);
        }
    }

    use codex_protocol::models::ResponseItem;

    // Full tool-call lifecycle: OutputItemAdded (registers the diff consumer) →
    // ToolCallInputDelta (argument fragment) → OutputItemDone (assembled call)
    // → terminal Completed with end_turn=false (turn continues so core runs the tool).
    let (added_name, added_call_id) = events
        .iter()
        .find_map(|ev| match ev {
            ResponseEvent::OutputItemAdded(ResponseItem::FunctionCall { name, call_id, .. }) => {
                Some((name.clone(), call_id.clone()))
            }
            _ => None,
        })
        .context("expected OutputItemAdded(FunctionCall)")?;
    assert_eq!(added_name, "search");
    assert_eq!(added_call_id, "call_123");

    let (delta_call_id, delta_text) = events
        .iter()
        .find_map(|ev| match ev {
            ResponseEvent::ToolCallInputDelta { call_id, delta, .. } => {
                Some((call_id.clone(), delta.clone()))
            }
            _ => None,
        })
        .context("expected a ToolCallInputDelta")?;
    assert_eq!(delta_call_id.as_deref(), Some("call_123"));
    assert!(delta_text.contains("test"));

    let (done_name, done_args) = events
        .iter()
        .find_map(|ev| match ev {
            ResponseEvent::OutputItemDone(ResponseItem::FunctionCall {
                name, arguments, ..
            }) => Some((name.clone(), arguments.clone())),
            _ => None,
        })
        .context("expected OutputItemDone(FunctionCall)")?;
    assert_eq!(done_name, "search");
    assert!(done_args.contains("test"));

    let end_turn = events.iter().find_map(|ev| match ev {
        ResponseEvent::Completed { end_turn, .. } => *end_turn,
        _ => None,
    });
    // finish_reason "tool_calls" => end_turn is false (turn continues). Absence
    // of a Completed event also fails this assertion.
    assert_eq!(end_turn, Some(false));
    Ok(())
}

// ============================================================================
// Test 6: Error Response Conversion
// ============================================================================

#[tokio::test]
async fn chat_completions_error_response_maps_correctly() -> Result<()> {
    // Chat Completions error responses map correctly to ApiError types.
    let error = codex_api::ChatError {
        error: codex_api::chat_response::ChatErrorDetail {
            message: "Invalid auth".to_string(),
            r#type: "authentication_error".to_string(),
            code: Some("invalid_api_key".to_string()),
        },
    };

    match codex_api::map_chat_error_to_api_error(error) {
        codex_api::ApiError::Api { status, .. } => assert_eq!(status, StatusCode::UNAUTHORIZED),
        other => panic!("Expected ApiError::Api with UNAUTHORIZED status, got {other:?}"),
    }
    Ok(())
}

#[tokio::test]
async fn rate_limit_error_maps_correctly() -> Result<()> {
    let error = codex_api::ChatError {
        error: codex_api::chat_response::ChatErrorDetail {
            message: "Rate limit exceeded".to_string(),
            r#type: "rate_limit_error".to_string(),
            code: Some("rate_limit_exceeded".to_string()),
        },
    };

    match codex_api::map_chat_error_to_api_error(error) {
        codex_api::ApiError::RateLimit(msg) => assert_eq!(msg, "Rate limit exceeded"),
        other => panic!("Expected ApiError::RateLimit, got {other:?}"),
    }
    Ok(())
}

// ============================================================================
// Test 7: Error Object Delivered Mid-Stream
// ============================================================================

#[tokio::test]
async fn chat_completions_error_in_stream_surfaces_api_error() -> Result<()> {
    // A provider may deliver an error object as a stream data frame. It must be
    // parsed and surfaced as an ApiError rather than silently dropped.
    let error_frame = serde_json::json!({
        "error": {
            "message": "Rate limit exceeded",
            "type": "rate_limit_error",
            "code": "rate_limit_exceeded"
        }
    });
    let serialized = serde_json::to_string(&error_frame)?;
    let body = format!("data: {serialized}\n\n");

    let transport = FixtureSseTransport::new(body);
    let client =
        codex_api::ChatCompletionsClient::new(transport, provider("openai"), Arc::new(NoAuth));

    let request = serde_json::json!({
        "model": "gpt-5",
        "messages": [{"role": "user", "content": "hi"}]
    });

    let mut stream = client
        .stream_request(
            request,
            ChatCompletionsOptions {
                compression: Compression::None,
                ..Default::default()
            },
        )
        .await?;

    let mut surfaced: Option<codex_api::ApiError> = None;
    while let Some(ev) = stream.next().await {
        if let Err(err) = ev {
            surfaced = Some(err);
            break;
        }
    }

    match surfaced {
        Some(codex_api::ApiError::RateLimit(msg)) => assert_eq!(msg, "Rate limit exceeded"),
        other => panic!("expected ApiError::RateLimit from stream, got {other:?}"),
    }
    Ok(())
}
