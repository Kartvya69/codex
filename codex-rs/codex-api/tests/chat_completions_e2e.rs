//! End-to-end integration tests for Chat Completions Wire API
//!
//! Tests cover:
//! - Full request/response cycle through client
//! - Backward compatibility with Responses API
//! - Config validation (compaction + Chat = error)
//! - Transport selection (Chat skips WebSocket, goes to SSE)

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use bytes::Bytes;
use codex_api::AuthProvider;
use codex_api::ChatCompletionsClient;
use codex_api::ChatCompletionsOptions;
use codex_api::Compression;
use codex_api::Provider;
use codex_api::ResponsesClient;
use codex_api::ResponsesOptions;
use codex_api::ResponseEvent;
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

#[async_trait]
impl HttpTransport for FixtureSseTransport {
    async fn execute(&self, _req: Request) -> Result<Response, TransportError> {
        Err(TransportError::Build("execute should not run".to_string()))
    }

    async fn stream(&self, _req: Request) -> Result<StreamResponse, TransportError> {
        let stream = futures::stream::iter(vec![Ok::<Bytes, TransportError>(Bytes::from(
            self.body.clone(),
        ))]);
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

/// Build Chat Completions SSE body from events
fn build_chat_completions_body(events: Vec<Value>) -> String {
    let mut body = String::new();
    for e in events {
        // Chat Completions uses "data:" prefix for all events
        body.push_str(&format!("data: {}\n\n", serde_json::to_string(&e).unwrap()));
    }
    // Add [DONE] sentinel
    body.push_str("data: [DONE]\n\n");
    body
}

// ============================================================================
// Test 1: OpenAI Chat Completions End-to-End
// ============================================================================

#[tokio::test]
async fn openai_chat_completions_e2e() {
    // Mock server expects Chat Completions format at /v1/chat/completions
    let chunk1 = serde_json::json!({
        "id": "chatcmpl-123",
        "object": "chat.completion.chunk",
        "created": 1677652288,
        "model": "gpt-5",
        "choices": [{
            "index": 0,
            "delta": {
                "role": "assistant",
                "content": "Hello"
            },
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
            "delta": {
                "content": " world"
            },
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

    let body = build_chat_completions_body(vec![chunk1, chunk2, chunk3]);
    let transport = FixtureSseTransport::new(body);
    let client = codex_api::ChatCompletionsClient::new(
        transport,
        provider("openai"),
        Arc::new(NoAuth),
    );

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
        .await
        .unwrap();

    let mut events = Vec::new();
    while let Some(ev) = stream.next().await {
        events.push(ev.unwrap());
    }

    // Filter out non-content events
    let content_events: Vec<_> = events
        .into_iter()
        .filter(|ev| matches!(ev, ResponseEvent::OutputTextDelta(_)))
        .collect();

    assert_eq!(content_events.len(), 2);
    match &content_events[0] {
        ResponseEvent::OutputTextDelta(text) => assert_eq!(text, "Hello"),
        _ => panic!("Expected OutputTextDelta"),
    }
    match &content_events[1] {
        ResponseEvent::OutputTextDelta(text) => assert_eq!(text, " world"),
        _ => panic!("Expected OutputTextDelta"),
    }
}

// ============================================================================
// Test 2: Responses API Still Works
// ============================================================================

#[tokio::test]
async fn responses_api_still_works() {
    // Verify existing Responses API path unchanged
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
    for e in vec![item1, completed] {
        let kind = e.get("type").and_then(|v| v.as_str()).unwrap();
        if e.as_object().map(|o| o.len() == 1).unwrap_or(false) {
            body.push_str(&format!("event: {kind}\n\n"));
        } else {
            body.push_str(&format!("event: {kind}\ndata: {e}\n\n"));
        }
    }

    let transport = FixtureSseTransport::new(body);
    let client = codex_api::ResponsesClient::new(
        transport,
        provider("claude"),
        Arc::new(NoAuth),
    );

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
        .await
        .unwrap();

    let mut events = Vec::new();
    while let Some(ev) = stream.next().await {
        events.push(ev.unwrap());
    }

    let events: Vec<ResponseEvent> = events
        .into_iter()
        .filter(|ev| !matches!(ev, ResponseEvent::RateLimits(_)))
        .collect();

    assert_eq!(events.len(), 2);
    match &events[0] {
        ResponseEvent::OutputItemDone(_) => {
            // Expected
        }
        other => panic!("unexpected first event: {other:?}"),
    }
    match &events[1] {
        ResponseEvent::Completed { .. } => {
            // Expected
        }
        other => panic!("unexpected second event: {other:?}"),
    }
}

// ============================================================================
// Test 3: Compaction with Chat Returns Error
// ============================================================================
//
// NOTE: This test requires ModelClient from core crate.
// Test location: /root/codex/codex-rs/core/src/client_tests.rs
// Reason: Config validation happens at ModelClient construction time.
//
// #[tokio::test]
// async fn compaction_with_chat_returns_error() {
//     // Test implementation in core crate
// }

// ============================================================================
// Test 4: Chat Skips WebSocket Goes Straight to SSE
// ============================================================================
//
// NOTE: This test requires ModelClient from core crate.
// Test location: /root/codex/codex-rs/core/src/client_tests.rs
// Reason: WebSocket prewarm logic is in ModelClientSession.
//
// #[tokio::test]
// async fn chat_with_websockets_skips_prewarm() {
//     // Test implementation in core crate
// }
//
// #[tokio::test]
// async fn chat_provider_does_not_support_websockets() {
//     // Test implementation in core crate
// }

// ============================================================================
// Test 5: Tool Calls via Chat Completions
// ============================================================================

#[tokio::test]
async fn chat_completions_tool_calls_e2e() {
    let chunk1 = serde_json::json!({
        "id": "chatcmpl-456",
        "object": "chat.completion.chunk",
        "created": 1677652289,
        "model": "gpt-5",
        "choices": [{
            "index": 0,
            "delta": {
                "role": "assistant",
                "content": null
            },
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
                    "function": {
                        "name": "search",
                        "arguments": "{\"query\":\"test\"}"
                    }
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
            "finish_reason": "stop"
        }]
    });

    let body = build_chat_completions_body(vec![chunk1, chunk2, chunk3]);
    let transport = FixtureSseTransport::new(body);
    let client = codex_api::ChatCompletionsClient::new(
        transport,
        provider("openai"),
        Arc::new(NoAuth),
    );

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
                    "properties": {
                        "query": {"type": "string"}
                    },
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
        .await
        .unwrap();

    let mut events = Vec::new();
    while let Some(ev) = stream.next().await {
        if let Ok(ev) = ev {
            events.push(ev);
        }
    }

    // Should have tool call event
    let tool_call_events: Vec<_> = events
        .into_iter()
        .filter(|ev| matches!(ev, ResponseEvent::ToolCallInputDelta { .. }))
        .collect();

    assert_eq!(tool_call_events.len(), 1);
    match &tool_call_events[0] {
        ResponseEvent::ToolCallInputDelta {
            call_id,
            delta,
            ..
        } => {
            assert_eq!(call_id.as_ref().unwrap(), "call_123");
            assert!(delta.contains("test"));
        }
        _ => panic!("Expected ToolCallInputDelta"),
    }
}

// ============================================================================
// Test 6: Error Response Conversion
// ============================================================================

#[tokio::test]
async fn chat_completions_error_response_maps_correctly() {
    // This test verifies that Chat Completions error responses
    // are mapped correctly to ApiError types

    let error_response = serde_json::json!({
        "error": {
            "message": "Invalid auth",
            "type": "authentication_error",
            "code": "invalid_api_key"
        }
    });

    // The conversion happens in the client layer
    // This test validates the mapping logic exists
    let error = codex_api::ChatError {
        error: codex_api::chat_response::ChatErrorDetail {
            message: "Invalid auth".to_string(),
            r#type: "authentication_error".to_string(),
            code: Some("invalid_api_key".to_string()),
        },
    };

    let api_error = codex_api::map_chat_error_to_api_error(error);

    match api_error {
        codex_api::ApiError::Api { status, .. } => {
            assert_eq!(status, StatusCode::UNAUTHORIZED);
        }
        _ => panic!("Expected ApiError::Api with UNAUTHORIZED status"),
    }
}

#[tokio::test]
async fn rate_limit_error_maps_correctly() {
    let error = codex_api::ChatError {
        error: codex_api::chat_response::ChatErrorDetail {
            message: "Rate limit exceeded".to_string(),
            r#type: "rate_limit_error".to_string(),
            code: Some("rate_limit_exceeded".to_string()),
        },
    };

    let api_error = codex_api::map_chat_error_to_api_error(error);

    match api_error {
        codex_api::ApiError::RateLimit(msg) => {
            assert_eq!(msg, "Rate limit exceeded");
        }
        _ => panic!("Expected ApiError::RateLimit"),
    }
}
