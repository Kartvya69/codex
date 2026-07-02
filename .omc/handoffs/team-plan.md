## Handoff: team-plan → team-exec

- **Decided**: Central conversion layer (Option A) for Chat↔Responses translation. WireApi::Chat added as peer variant. Azure defaults to Responses for backward compat. Chat always uses HTTP/SSE (no WebSocket). Compaction fails fast for Chat providers. SSE events mapped 1:1 for text, buffered for tool calls. Error types mapped to ApiError variants. TDD enforced for all phases.
- **Rejected**: Provider-specific paths (Option B) — duplicate logic, harder to test. Chat as default for Azure — breaks existing configs. WebSocket for Chat — Chat Completions API doesn't support it.
- **Risks**: Chat format drift when OpenAI updates API (mitigate: versioned types + tests). Tool call buffering under high concurrency (mitigate: bounded buffer at 1600). Non-convertible types (LocalShellCall, ToolSearchCall, etc.) will error for Chat providers.
- **Files**: Plan at .omc/plans/restore-chat-completions-api.md (946 lines). Key source files: model-provider-info/src/lib.rs, core/src/client.rs, protocol/src/models.rs, codex-api/src/common.rs
- **Remaining**: All 6 implementation phases. Workers assigned with dependency chain: T1→T2+T3→T4+T5→T6.
