# Restore Chat Completions Wire API

## Plan Summary

**Scope:**
- 6 phases across 15 core files
- Estimated complexity: MEDIUM

**Key Deliverables:**
1. WireApi enum with `Chat` variant restored
2. Proto enum supporting both `Responses` and `Chat` wire APIs
3. Client routing logic for chat completions format
4. Request/response conversion layer with SSE event mapping
5. Error response conversion module
6. Provider-specific configuration support
7. Compaction incompatibility handling
8. Transport selection logic (HTTP/SSE only for Chat)
9. Comprehensive test coverage
10. Conversion fidelity matrix

---

## RALPLAN-DR Summary

### Principles (3-5)
1. **Dual Wire API Support**: Chat Completions and Responses APIs coexist as peers
2. **Provider Routing**: Providers declare which wire API they use; client routes accordingly
3. **TDD Discipline**: Tests written before implementation for all components
4. **Backward Compatibility**: Existing `wire_api = "responses"` configs work unchanged
5. **Type Safety**: Exhaustive matching, no silent fallbacks

### Decision Drivers (Top 3)
1. **Provider Diversity**: OpenAI/Azure use Chat Completions; Claude uses Responses; single binary must support both
2. **Migration Path**: Users migrating from OpenAI SDKs expect Chat Completions format
3. **Error Clarity**: Unsupported provider/wire-api combinations must fail with actionable errors

### Viable Options

#### Option A: Central Conversion Layer
**Pros:**
- Single place for format translation
- Easier to test conversion logic in isolation
- Clear separation between protocol and provider

**Cons:**
- Additional runtime overhead for conversion
- Another layer to maintain when formats change

#### Option B: Provider-Specific Paths
**Pros:**
- No conversion overhead; direct provider mapping
- Each provider handles its native format
- More flexible for provider-specific quirks

**Cons:**
- Duplicate logic across providers
- Harder to ensure consistent behavior

**Decision: Option A - Central Conversion Layer**

The conversion overhead is minimal compared to the benefits of having a single, testable translation layer. Provider-specific quirks can still be handled through provider-level adapters, but the core Chat Completions ↔ Responses translation lives in one place.

### Additional Decision: Azure Default Wire API
Azure defaults to `Responses` for backward compatibility. Users who want Chat must explicitly set `wire_api = "chat"` in config. This preserves existing Azure deployments.

---

## Phase Breakdown

### Phase 1: Restore WireApi Enum Variants

**Objective:** Add `Chat` variant to `WireApi` enum and update deserialization logic.

**Files:**
- `/root/codex/codex-rs/model-provider-info/src/lib.rs`
- `/root/codex/codex-rs/config/src/thread_config/proto/codex.thread_config.v1.rs`

**Pre-Implementation Tests:**
```rust
// In model-provider-info/src/lib.rs tests
#[test]
fn deserialize_wire_api_chat() {
    let json = r#"{"wire_api": "chat"}"#;
    let config: WireApiConfig = serde_json::from_str(json).unwrap();
    assert_eq!(config.wire_api, WireApi::Chat);
}

#[test]
fn deserialize_wire_api_responses() {
    let json = r#"{"wire_api": "responses"}"#;
    let config: WireApiConfig = serde_json::from_str(json).unwrap();
    assert_eq!(config.wire_api, WireApi::Responses);
}

#[test]
fn deserialize_wire_api_unspecified_defaults_to_responses() {
    let json = r#"{"wire_api": "unspecified"}"#;
    let config: WireApiConfig = serde_json::from_str(json).unwrap();
    assert_eq!(config.wire_api, WireApi::Responses);
}
```

**Implementation Tasks:**
1. Add `Chat = 2` variant to `WireApi` enum
2. Update custom deserializer to accept `"chat"` and return `WireApi::Chat`
3. Remove `CHAT_WIRE_API_REMOVED_ERROR`; replace with proper variant
4. Update `fmt::Display` impl to handle `Chat` -> `"chat"`
5. Update proto enum: add `Chat = 2`
6. Update proto conversion functions (From/Into impls)
7. Update default: `Responses` remains default

**Acceptance Criteria:**
- [ ] `WireApi::Chat` deserializes from `"chat"`
- [ ] `WireApi::Responses` still deserializes from `"responses"`
- [ ] Proto enum has `Unspecified = 0, Responses = 1, Chat = 2`
- [ ] No compilation errors
- [ ] All existing tests pass

---

### Phase 2: Provider Configuration Layer

**Objective:** Allow providers to declare their supported/default wire API.

**Files:**
- `/root/codex/codex-rs/model-provider-info/src/lib.rs`
- `/root/codex/codex-rs/codex-api/src/provider.rs`
- Provider-specific config files (OpenAI, Bedrock, Ollama, LMStudio)

**Pre-Implementation Tests:**
```rust
#[test]
fn openai_defaults_to_chat_completions() {
    let provider = ModelProvider::OpenAI;
    assert_eq!(provider.default_wire_api(), WireApi::Chat);
}

#[test]
fn claude_defaults_to_responses() {
    let provider = ModelProvider::Claude;
    assert_eq!(provider.default_wire_api(), WireApi::Responses);
}

#[test]
fn azure_defaults_to_responses_for_backward_compat() {
    let provider = ModelProvider::Azure;
    assert_eq!(provider.default_wire_api(), WireApi::Responses);
}

#[test]
fn provider_supports_wire_api() {
    assert!(ModelProvider::OpenAI.supports_wire_api(WireApi::Chat));
    assert!(ModelProvider::OpenAI.supports_wire_api(WireApi::Responses));
}

#[test]
fn chat_provider_does_not_support_websockets() {
    let info = ModelProviderInfo {
        wire_api: WireApi::Chat,
        supports_websockets: true,
        ..Default::default()
    };
    assert!(info.validate().is_err(), "Chat with websockets should fail");
}
```

**Implementation Tasks:**
1. Add `default_wire_api()` method to `ModelProvider` enum
2. Add `supports_wire_api()` method for capability checking
3. Update `ModelProviderInfo::validate()` to reject `wire_api = Chat` with `supports_websockets = true`
4. Add validation: error if config specifies unsupported wire API for provider
5. Update provider docs to reflect wire API support
6. Add `supports_websockets()` check: returns `false` when `wire_api = Chat`

**Provider Defaults:**
- OpenAI: `Chat` (explicit opt-in for Responses)
- Azure: `Responses` (backward compat)
- Bedrock: `Chat` (for GPT models)
- Claude: `Responses` (only option)
- Ollama/LMStudio: `Chat` (OpenAI-compatible)

**Acceptance Criteria:**
- [ ] Each provider declares its default wire API
- [ ] Capability check returns `bool` for provider/wire-api pairs
- [ ] Config validation fails with clear error for unsupported combinations
- [ ] `Chat + websockets` fails validation with actionable error
- [ ] OpenAI/Azure default to `Chat`, Claude defaults to `Responses`
- [ ] All tests pass

---

### Phase 3: Client Routing Logic and Transport Selection

**Objective:** Route requests to appropriate wire API handler based on configuration. Handle HTTP/SSE vs WebSocket selection.

**Files:**
- `/root/codex/codex-rs/core/src/client.rs`
- `/root/codex/codex-rs/core/src/lib.rs` (if new modules needed)
- `/root/codex/codex-rs/codex-api/src/endpoint/mod.rs`

**Pre-Implementation Tests:**
```rust
#[tokio::test]
async fn client_routes_to_chat_completions() {
    let config = ClientConfig {
        wire_api: WireApi::Chat,
        provider: ModelProvider::OpenAI,
        ..default()
    };
    let client = Client::new(config).await.unwrap();

    // Mock server expects Chat Completions format
    let mock = mock("POST", "/v1/chat/completions")
        .with_status(200)
        .with_body(mock_chat_response())
        .create();

    let result = client.complete(&request).await.unwrap();
    mock.assert();
}

#[tokio::test]
async fn client_routes_to_responses() {
    let config = ClientConfig {
        wire_api: WireApi::Responses,
        provider: ModelProvider::Claude,
        ..default()
    };
    let client = Client::new(config).await.unwrap();

    let mock = mock("POST", "/v1/responses")
        .with_status(200)
        .with_body(mock_responses_response())
        .create();

    let result = client.complete(&request).await.unwrap();
    mock.assert();
}

#[test]
fn chat_wire_api_skips_websocket_prewarm() {
    let client = ModelClient::with_wire_api(WireApi::Chat);
    assert!(!client.should_prewarm_websocket());
}

#[test]
fn chat_provider_does_not_support_websockets() {
    let provider = Provider::from_model_provider_info(ModelProviderInfo {
        wire_api: WireApi::Chat,
        ..Default::default()
    });
    assert_eq!(provider.supports_websockets(), false);
}
```

**Transport Selection Logic:**
- `WireApi::Chat` always uses HTTP/SSE (no WebSocket support for Chat Completions API)
- Client skips WebSocket prewarm when `wire_api = "chat"`
- `supports_websockets()` field returns `false` for Chat-configured providers
- No fallback needed since there's only one transport for Chat

**Compaction Incompatibility Handling:**
- Add validation in `ModelClientState::new()` or equivalent
- Error message: "Compaction is not supported with wire_api = 'chat'. Use wire_api = 'responses' or disable compaction."
- Fail-fast at config validation time, not runtime

**Implementation Tasks:**
1. Add `match` on `WireApi` in `Client::complete()` or equivalent method
2. Create stub handlers for each wire API
3. Update endpoint URL construction (Chat: `/v1/chat/completions`, Responses: `/v1/responses`)
4. Add exhaustive match to prevent missing variants
5. Add WebSocket prewarm skip logic for `WireApi::Chat`
6. Add compaction incompatibility check
7. Add `supports_websockets()` method to `Provider` that returns `false` for Chat

**Acceptance Criteria:**
- [ ] Client routes to correct endpoint based on `WireApi`
- [ ] Chat uses `/v1/chat/completions`, Responses uses `/v1/responses`
- [ ] Compiler enforces exhaustive match
- [ ] Routing tests pass with wiremock
- [ ] WebSocket prewarm skipped for `WireApi::Chat`
- [ ] `supports_websockets()` returns `false` for Chat providers
- [ ] Compaction with `WireApi::Chat` fails with clear error at config time

---

### Phase 4: Request Type Conversion Layer with Tool Call Rules

**Objective:** Convert between internal protocol and Chat Completions request format.

**Files:**
- `/root/codex/codex-rs/codex-api/src/request.rs` (new module)
- `/root/codex/codex-rs/protocol/src/items.rs`
- `/root/codex/codex-rs/codex-api/src/common.rs`

**Tool Call Conversion Rules:**

**Direct Mappings:**
- `tool_calls[].id` → `call_id` field
- `tool_calls[].function.name` → `name` field

**Namespace Synthesis:**
- Use `None` for Chat-sourced tool calls (Chat doesn't have namespaces)

**Parallel Tool Calls:**
- Chat returns array → convert to sequence of `ResponseItem::FunctionCall` items
- Each tool call gets its own `ResponseItem`

**Non-Convertible Scenarios (Error with Clear Message):**
The following ResponseItem types CANNOT convert from Chat Completions:
- `LocalShellCall` - no Chat equivalent
- `ToolSearchCall` - no Chat equivalent
- `WebSearchCall` - no Chat equivalent
- `ImageGenerationCall` - no Chat equivalent
- `Compaction` - no Chat equivalent

**Pre-Implementation Tests:**
```rust
#[test]
fn convert_request_to_chat_completions() {
    let internal = InternalRequest {
        messages: vec![
            Message::user("Hello"),
            Message::assistant("Hi there"),
        ],
        tools: Some(vec![tool_definition()]),
        ..default()
    };

    let chat = to_chat_completions_request(&internal).unwrap();

    assert_eq!(chat.messages.len(), 2);
    assert_eq!(chat.messages[0].role, "user");
    assert_eq!(chat.messages[0].content, "Hello");
}

#[test]
fn convert_tool_definition_to_chat_function() {
    let tool = Tool {
        name: "calculator".to_string(),
        description: Some("Does math".to_string()),
        input_schema: json!({"type": "object"}),
    };

    let function = to_function_definition(&tool).unwrap();

    assert_eq!(function.name, "calculator");
    assert_eq!(function.description, "Does math");
}

#[test]
fn parallel_tool_calls_convert_to_sequence() {
    let chat_response = ChatCompletionResponse {
        choices: vec![Choice {
            message: ChatMessage {
                role: "assistant".to_string(),
                content: None,
                tool_calls: Some(vec![
                    ToolCall {
                        id: "call_1".to_string(),
                        function: FunctionCall {
                            name: "tool_a".to_string(),
                            arguments: r#"{"x": 1}"#.to_string(),
                        },
                    },
                    ToolCall {
                        id: "call_2".to_string(),
                        function: FunctionCall {
                            name: "tool_b".to_string(),
                            arguments: r#"{"y": 2}"#.to_string(),
                        },
                    },
                ]),
            },
            ..default()
        }],
        ..default()
    };

    let items = from_chat_tool_calls(&chat_response.choices[0].message.tool_calls.unwrap()).unwrap();
    assert_eq!(items.len(), 2);
    assert!(matches!(items[0], ResponseItem::FunctionCall { name, .. } if name == "tool_a"));
    assert!(matches!(items[1], ResponseItem::FunctionCall { name, .. } if name == "tool_b"));
}

#[test]
fn chat_sourced_tool_calls_have_no_namespace() {
    let tool_call = ResponseItem::FunctionCall {
        name: "calculator".to_string(),
        namespace: None, // Chat doesn't have namespaces
        arguments: r#"{"x": 1}"#.to_string(),
        call_id: "call_1".to_string(),
        ..Default::default()
    };
    assert!(tool_call.namespace.is_none());
}
```

**Implementation Tasks:**
1. Create `chat_completions` module with request/response types
2. Implement `to_chat_completions_request()` conversion function
3. Implement `tool_to_function_definition()` for tool use translation
4. Add tests for edge cases (no tools, system prompts, streaming)
5. Add parallel tool call conversion logic
6. Add namespace handling (always `None` for Chat-sourced calls)
7. Add error for non-convertible tool types

**Acceptance Criteria:**
- [ ] Internal request converts to Chat Completions format
- [ ] Messages array preserves order and roles
- [ ] Tools convert to `functions` array with correct schema
- [ ] System messages handled correctly
- [ ] Empty tools handled gracefully
- [ ] Parallel tool calls convert to sequence of `FunctionCall` items
- [ ] Chat-sourced tool calls have `namespace = None`
- [ ] Non-convertible tool types return clear error
- [ ] All conversion tests pass

---

### Phase 5: Response Type Conversion Layer with SSE Event Mapping

**Objective:** Convert Chat Completions responses back to internal protocol format.

**Files:**
- `/root/codex/codex-rs/codex-api/src/response.rs` (new module)
- `/root/codex/codex-rs/protocol/src/items.rs`
- `/root/codex/codex-rs/codex-api/src/common.rs`

**SSE Event Mapping Contract:**

| Chat Completions SSE Event | ResponseEvent | Notes |
|----------------------------|---------------|-------|
| `chat.completion.chunk` with `choices[].delta.content` | `ResponseEvent::OutputTextDelta` | 1:1 mapping, emit per chunk |
| `choices[].delta.tool_calls` | `ResponseEvent::ToolCallInputDelta` | Buffer partial `function.arguments` deltas |
| `choices[].finish_reason = "stop"` | `ResponseEvent::Completed` | Stream termination |
| `[DONE]` sentinel | Stream termination | End of stream |

**Text Aggregation Strategy:**
- 1:1 mapping (emit per chunk, no batching)
- Each `delta.content` becomes its own `OutputTextDelta` event

**Tool Call Delta Aggregation:**
- Buffer partial `function.arguments` deltas
- Emit complete tool call when all arguments received
- Use bounded buffering (similar to `RESPONSE_STREAM_CHANNEL_CAPACITY: 1600`)

**Error Response Conversion:**

Chat error format:
```json
{
  "error": {
    "message": "...",
    "type": "...",
    "code": "..."
  }
}
```

**Error Type Mapping:**
| Chat Error Type | Codex `ApiError` | HTTP Status |
|-----------------|------------------|-------------|
| `invalid_request_error` | `ApiError::Api { status, message }` | 400 |
| `authentication_error` | `ApiError::Api { status: 401, message }` | 401 |
| `rate_limit_error` | `ApiError::RateLimitExceeded` | 429 |
| `model_not_found_error` | `ApiError::Api { status: 404, message }` | 404 |
| `context_length_exceeded` | `ApiError::ContextWindowExceeded` | 413/400 |
| `server_error` | `ApiError::Api { status: 500, message }` | 500 |

HTTP status codes preserved (429, 401, 403, 404, 500).

**Pre-Implementation Tests:**
```rust
#[test]
fn convert_chat_response_to_internal() {
    let chat_response = ChatCompletionResponse {
        id: "chatcmpl-123".to_string(),
        choices: vec![Choice {
            message: ChatMessage {
                role: "assistant".to_string(),
                content: Some("Hello!".to_string()),
                tool_calls: None,
            },
            finish_reason: "stop".to_string(),
        }],
        usage: Some(Usage {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
        }),
    };

    let internal = from_chat_completions_response(chat_response).unwrap();

    assert_eq!(internal.content.as_ref().unwrap().text, "Hello!");
    assert_eq!(internal.usage.prompt_tokens, 10);
}

#[test]
fn convert_tool_call_from_chat_response() {
    let chat_response = ChatCompletionResponse {
        choices: vec![Choice {
            message: ChatMessage {
                role: "assistant".to_string(),
                content: None,
                tool_calls: Some(vec![ToolCall {
                    id: "call_123".to_string(),
                    function: FunctionCall {
                        name: "calculator".to_string(),
                        arguments: r#"{"x": 1, "y": 2}"#.to_string(),
                    },
                }]),
            },
            ..default()
        }],
        ..default()
    };

    let internal = from_chat_completions_response(chat_response).unwrap();

    assert!(internal.tool_use_requested);
    assert_eq!(internal.tool_calls.as_ref().unwrap()[0].name, "calculator");
    assert_eq!(internal.tool_calls.as_ref().unwrap()[0].call_id, "call_123");
}

#[test]
fn sse_text_delta_emits_per_chunk() {
    let chunk = ChatCompletionChunk {
        choices: vec![ChunkChoice {
            delta: ChunkDelta {
                content: Some("Hello".to_string()),
                tool_calls: None,
            },
        }],
    };

    let events = map_sse_chunk_to_events(chunk);
    assert_eq!(events, vec![ResponseEvent::OutputTextDelta("Hello".to_string())]);
}

#[test]
fn sse_tool_call_delta_buffers_arguments() {
    let chunk1 = ChatCompletionChunk {
        choices: vec![ChunkChoice {
            delta: ChunkDelta {
                content: None,
                tool_calls: Some(vec![ToolCallDelta {
                    index: 0,
                    id: Some("call_1".to_string()),
                    function: FunctionCallDelta {
                        name: Some("calc".to_string()),
                        arguments: Some('{'.to_string()),
                    },
                }]),
            },
        }],
    };

    let chunk2 = ChatCompletionChunk {
        choices: vec![ChunkChoice {
            delta: ChunkDelta {
                content: None,
                tool_calls: Some(vec![ToolCallDelta {
                    index: 0,
                    id: None,
                    function: FunctionCallDelta {
                        name: None,
                        arguments: Some(r#""x": 1}"#.to_string()),
                    },
                }]),
            },
        }],
    };

    let mut buffer = ToolCallBuffer::new();
    let events1 = buffer.process_chunk(&chunk1);
    assert!(events1.is_empty(), "Partial arguments buffered");

    let events2 = buffer.process_chunk(&chunk2);
    assert!(!events2.is_empty(), "Complete tool call emitted");
}

#[test]
fn error_response_maps_correctly() {
    let chat_error = ChatError {
        error: ChatErrorDetail {
            message: "Invalid request".to_string(),
            type_: "invalid_request_error".to_string(),
            code: None,
        },
    };

    let api_error = map_chat_error_to_api_error(chat_error, 400);
    assert!(matches!(api_error, ApiError::Api { status: 400, .. }));
}

#[test]
fn rate_limit_error_maps_correctly() {
    let chat_error = ChatError {
        error: ChatErrorDetail {
            message: "Rate limit exceeded".to_string(),
            type_: "rate_limit_error".to_string(),
            code: None,
        },
    };

    let api_error = map_chat_error_to_api_error(chat_error, 429);
    assert!(matches!(api_error, ApiError::RateLimitExceeded));
}
```

**Implementation Tasks:**
1. Implement `from_chat_completions_response()` conversion function
2. Handle `tool_calls` → internal tool use format with namespace = None
3. Convert `usage` stats to internal format
4. Map `finish_reason` to internal stop reason
5. Create SSE event mapper for streaming responses
6. Implement bounded buffering for tool call deltas (use `RESPONSE_STREAM_CHANNEL_CAPACITY` pattern)
7. Implement error response converter with type mapping
8. Add HTTP status code preservation
9. Handle streaming responses (chunk aggregation)

**Acceptance Criteria:**
- [ ] Chat text content maps to internal content
- [ ] Tool calls convert to internal tool use format with namespace = None
- [ ] Usage statistics preserved
- [ ] Finish reasons mapped correctly
- [ ] SSE text deltas emit 1:1 per chunk
- [ ] Tool call deltas buffered and emitted when complete
- [ ] Bounded buffering uses capacity similar to Responses API
- [ ] Error types mapped correctly to ApiError variants
- [ ] HTTP status codes preserved
- [ ] All conversion tests pass

---

### Phase 6: End-to-End Integration

**Objective:** Wire everything together with full provider support.

**Files:**
- `/root/codex/codex-rs/core/src/client.rs`
- `/root/codex/codex-rs/config/src/thread_config/remote.rs`
- Integration test files
- Conversion fidelity matrix (document)

**Conversion Fidelity Matrix:**

| Feature | Chat → Responses | Responses → Chat | Notes |
|---------|-------------------|-------------------|-------|
| Text messages | ✅ Full | ✅ Full | 1:1 mapping |
| System prompts | ✅ Full | ✅ Full | Preserved |
| Tool calls (function) | ✅ Full | ✅ Full | namespace = None for Chat |
| Parallel tool calls | ✅ Full | ✅ Full | Array → sequence |
| Local shell calls | ❌ Not supported | N/A | No Chat equivalent |
| Tool search | ❌ Not supported | N/A | No Chat equivalent |
| Web search | ❌ Not supported | N/A | No Chat equivalent |
| Image generation | ❌ Not supported | N/A | No Chat equivalent |
| Compaction | ❌ Not supported | N/A | No Chat equivalent |
| Streaming | ✅ Full (SSE) | ✅ Full (SSE) | Different event names, same semantics |
| Usage stats | ✅ Full | ✅ Full | Prompt/completion tokens preserved |
| Error responses | ✅ Full (mapped) | ✅ Full (mapped) | Type translation applied |

**Integration Tests:**
```rust
// Integration test in tests/chat_completions_e2e.rs
#[tokio::test]
async fn openai_chat_completions_e2e() {
    let mut server = MockServer::start().await;
    let mock = server
        .mock("POST", "/v1/chat/completions")
        .match_header("authorization", "Bearer test-key")
        .with_status(200)
        .with_body(full_chat_response())
        .create();

    let config = Config {
        provider: ModelProvider::OpenAI,
        wire_api: WireApi::Chat,
        api_key: "test-key".to_string(),
        base_url: server.url(),
        ..default()
    };

    let result = complete_with_config(config).await.unwrap();

    mock.assert();
    assert_eq!(result.content.unwrap().text, "Test response");
}

#[tokio::test]
async fn responses_api_still_works() {
    // Verify existing Responses API path unchanged
    let mut server = MockServer::start().await;
    let mock = server
        .mock("POST", "/v1/responses")
        .with_status(200)
        .with_body(full_responses_response())
        .create();

    let config = Config {
        provider: ModelProvider::Claude,
        wire_api: WireApi::Responses,
        api_key: "test-key".to_string(),
        base_url: server.url(),
        ..default()
    };

    let result = complete_with_config(config).await.unwrap();

    mock.assert();
    assert!(result.content.is_some());
}

#[tokio::test]
async fn compaction_with_chat_returns_error() {
    let config = Config {
        provider: ModelProvider::OpenAI,
        wire_api: WireApi::Chat,
        enable_compaction: true,
        ..default()
    };

    let result = ModelClient::new(config).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Compaction is not supported"));
}

#[tokio::test]
async fn chat_with_websockets_skips_prewarm() {
    let config = Config {
        provider: ModelProvider::OpenAI,
        wire_api: WireApi::Chat,
        supports_websockets: false,
        ..default()
    };

    let client = ModelClient::new(config).await.unwrap();
    assert!(!client.should_prewarm_websocket());
}
```

**Implementation Tasks:**
1. Update client to use conversion layers based on `WireApi`
2. Ensure configuration loading properly sets wire API
3. Add integration tests for each provider/wire-api combination
4. Update documentation with examples
5. Verify backward compatibility with existing configs
6. Document conversion fidelity matrix
7. Add conversion audit test phase

**Acceptance Criteria:**
- [ ] OpenAI with `wire_api = "chat"` works end-to-end
- [ ] Claude with `wire_api = "responses"` works unchanged
- [ ] Cross-provider (e.g., Azure with Chat Completions) works
- [ ] Unsupported combination fails with clear error
- [ ] Compaction with `wire_api = "chat"` fails at config time
- [ ] WebSocket prewarm skipped for Chat providers
- [ ] All integration tests pass
- [ ] Existing configs continue to work without modification
- [ ] Conversion fidelity matrix documented
- [ ] Documentation updated with examples

---

## Dependency Graph

```
Phase 1 (Enum Restoration)
    ├─> Phase 2 (Provider Config)
    │       └─> Phase 3 (Client Routing + Transport + Compaction)
    │               ├─> Phase 4 (Request Conversion + Tool Call Rules)
    │               └─> Phase 5 (Response Conversion + SSE Mapping + Error Mapping)
    │                       └─> Phase 6 (E2E Integration + Fidelity Matrix)
    └─> Phase 3 (Client Routing) [direct dependency for match arm]
```

**Critical Path:** Phase 1 → Phase 3 → Phase 4 → Phase 6

**Parallel Opportunities:**
- Phase 2 and Phase 4 can proceed in parallel after Phase 1
- Phase 5 can start once Phase 4 is underway
- Integration tests (Phase 6 prep) can be written alongside Phases 3-5

---

## Test Strategy

### Unit Tests (Phases 1-5)
- **Location:** `#[cfg(test)]` modules in source files
- **Coverage:** Enum variants, conversion functions, routing logic, SSE mapping
- **Tools:** Standard Rust test framework, `wiremock` for HTTP

### Integration Tests (Phase 6)
- **Location:** `tests/` directories
- **Coverage:** Full request/response cycles with mock servers
- **Tools:** `wiremock`, `tokio::test`

### Property-Based Tests
- **Location:** `proptest` modules for conversion functions
- **Coverage:** Round-trip conversions preserve data
- **Tools:** `proptest` crate

### Conversion Audit Tests
- **Location:** `tests/conversion_audit.rs`
- **Coverage:** All documented conversion paths in fidelity matrix
- **Tools:** Custom test helpers for matrix validation

---

## Risk Assessment

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Chat Completions format drifts (OpenAI updates) | Medium | Medium | Version our request/response types; add specific tests for format changes |
| Conversion layer introduces performance regression | Low | Low | Benchmark conversion functions; optimize hot paths if needed |
| Existing configs break with default wire API changes | Low | High | Keep `Responses` as default for all providers; explicit opt-in for Chat |
| Streaming responses have edge cases | Medium | Medium | Add dedicated streaming tests; mock chunked responses; use bounded buffering |
| Provider-specific quirks not captured by central layer | High | Medium | Design layer to allow provider-level overrides/extensions |
| SSE event ordering differs between providers | Medium | High | Document ordering assumptions; add ordering tests |
| Tool call buffering unbounded memory growth | Low | High | Use bounded buffer with `RESPONSE_STREAM_CHANNEL_CAPACITY` pattern |

---

## Conversion Fidelity Matrix (Full Documentation)

### WILL Convert (Full Support)
- Text messages (user/assistant/system)
- Single tool calls (function type)
- Parallel tool calls (array → sequence)
- Streaming responses (SSE, different event names)
- Usage statistics (prompt/completion/total tokens)
- Error responses (type mapping applied)

### WILL NOT Convert (Error with Clear Message)
- `LocalShellCall` → "Local shell calls are not supported with wire_api='chat'"
- `ToolSearchCall` → "Tool search is not supported with wire_api='chat'"
- `WebSearchCall` → "Web search is not supported with wire_api='chat'"
- `ImageGenerationCall` → "Image generation is not supported with wire_api='chat'"
- `Compaction` → "Compaction is not supported with wire_api='chat'"

### LOSSY Conversion (Document Loss)
- Namespace information: Chat-sourced tool calls always have `namespace = None`
  - Loss: Response API namespace metadata not preserved
  - Impact: Minimal for Chat-only providers
- Tool call ordering: Parallel calls become sequential in ResponseItem array
  - Loss: Original parallelism indicator
  - Impact: Semantic meaning preserved, execution order implied

---

## Open Questions (RESOLVED)

1. **Streaming Behavior:** Chat Completions uses SSE with different event names (`chat.completion.chunk` vs `response.output_item.delta`). **RESOLVED:** 1:1 mapping with dedicated SSE event mapper; text deltas emit per chunk; tool call deltas use bounded buffering.

2. **Tool Use Semantics:** Chat Completions `tool_calls` vs Responses `tool_use`. **RESOLVED:** 
   - `tool_calls[].id` → `call_id` field (direct mapping)
   - `tool_calls[].function.name` → `name` field (direct mapping)
   - `namespace` synthesis: use `None` for Chat-sourced tool calls
   - Parallel tool calls: array → sequence of `ResponseItem::FunctionCall`
   - Non-convertible types: `LocalShellCall`, `ToolSearchCall`, `WebSearchCall`, `ImageGenerationCall`, `Compaction` return clear errors

3. **Default Wire API for Azure:** Azure OpenAI supports both. **RESOLVED:** Azure defaults to `Responses` for backward compatibility. Users must explicitly set `wire_api = "chat"` to use Chat Completions.

4. **Error Response Mapping:** Chat error format differs from Responses. **RESOLVED:** Error conversion module maps Chat error types to Codex error types with HTTP status preservation.

5. **Compaction Incompatibility:** Chat doesn't support compaction. **RESOLVED:** Fail-fast at config validation with error "Compaction is not supported with wire_api = 'chat'. Use wire_api = 'responses' or disable compaction."

6. **WebSocket/SSE Transport Selection:** Chat Completions API doesn't support WebSocket. **RESOLVED:** `WireApi::Chat` always uses HTTP/SSE; client skips WebSocket prewarm; `supports_websockets` returns `false`.

---

## Architecture Decision Record (ADR)

### Decision
Restore Chat Completions as a peer wire API alongside Responses, using a central conversion layer to translate between the two formats.

### Drivers
1. Provider diversity: OpenAI/Azure use Chat Completions; Claude uses Responses
2. User migration path from OpenAI SDKs
3. Single binary supporting multiple providers

### Alternatives Considered
1. **Central conversion layer** (chosen): Single place for translation, easier to test
2. **Provider-specific paths**: No overhead but duplicates logic

### Why Chosen
Central conversion layer provides clear separation of concerns and testability with minimal runtime overhead. Provider-specific quirks can still be handled through provider-level adapters.

### Consequences
- **Positive:** Easier to test, single source of truth for format translation
- **Negative:** Additional conversion overhead (minimal), another layer to maintain
- **Neutral:** Requires maintaining two request/response type definitions

### Follow-ups
1. Document Chat Completions format thoroughly
2. Add benchmark for conversion layer performance
3. Consider plugin system for provider-specific format extensions
4. Monitor OpenAI API changes for format drift

---

## Success Criteria

1. ✅ `WireApi::Chat` variant exists and deserializes correctly
2. ✅ Providers declare their supported wire APIs (OpenAI/Azure default to Chat, Claude to Responses)
3. ✅ Client routes to correct endpoint (`/v1/chat/completions` or `/v1/responses`)
4. ✅ Request/response conversion preserves all data (messages, tools, usage)
5. ✅ SSE event mapping handles text deltas and tool call deltas with bounded buffering
6. ✅ Error responses map correctly to ApiError variants
7. ✅ Compaction with `wire_api = "chat"` fails at config time
8. ✅ WebSocket prewarm skipped for Chat providers
9. ✅ All tests pass (unit, integration, property-based, conversion audit)
10. ✅ Existing `wire_api = "responses"` configs work unchanged
11. ✅ OpenAI/Azure work with `wire_api = "chat"`
12. ✅ Claude works with `wire_api = "responses"`
13. ✅ Unsupported combinations fail with clear error messages
14. ✅ Documentation updated with examples
15. ✅ Conversion fidelity matrix documented

---

## Post-Implementation Verification

```bash
# Unit tests
cargo nextest run --workspace --failure-output immediate

# Integration tests
cargo nextest run --workspace-integration --failure-output immediate

# Provider-specific smoke tests
cargo test --test openai_chat_completions
cargo test --test claude_responses

# Conversion audit tests
cargo test --test conversion_audit

# Backward compatibility check
cargo test --test backward_compat

# Performance benchmarks
cargo bench --bench conversion_layer
```
