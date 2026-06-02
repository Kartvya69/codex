# Open Questions

## restore-chat-completions-api - 2026-06-02

- [ ] **Streaming Behavior:** Should Chat Completions streaming use SSE (Server-Sent Events) with same wire format as Responses API, or is adapter logic needed?
- [ ] **Tool Use Semantics:** Chat Completions `tool_calls` vs Responses `tool_use` — need to clarify exact mapping for all tool use scenarios (parallel, nested, etc.)
- [ ] **Default Wire API for Azure:** Azure OpenAI supports both Chat Completions and Responses — what should the default be?
- [ ] **Error Response Mapping:** Chat Completions error format differs from Responses — need detailed error conversion spec
