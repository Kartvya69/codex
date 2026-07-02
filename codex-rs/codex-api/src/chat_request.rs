//! Chat Completions API request types and conversion
//!
//! This module handles converting internal protocol request types to
//! Chat Completions API request format.

use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ReasoningEffort;
use serde::Serialize;
use serde_json::Value;
use serde_json::json;

use crate::common::Reasoning;

// ============================================================================
// Chat Completions API Types
// ============================================================================

/// Chat Completions API request
#[derive(Debug, Clone, Serialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ChatTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Preferred output-token budget for reasoning/o-series models
    /// (`max_completion_tokens` in the Chat Completions API).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_completion_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub n: Option<u32>,
    /// Always `Some(true)` for the streaming transport used by codex-api.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    /// Requested alongside `stream: true` so usage stats are delivered in the
    /// final chunk and surfaced on the terminal `Completed` event.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<StreamOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<Stop>,
    /// Reasoning effort for o-series / reasoning models.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
}

/// `stream_options` payload for the Chat Completions API.
#[derive(Debug, Clone, Serialize)]
pub struct StreamOptions {
    pub include_usage: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ChatMessageToolCall>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatTool {
    pub r#type: String,
    pub function: FunctionDefinition,
}

#[derive(Debug, Clone, Serialize)]
pub struct FunctionDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatMessageToolCall {
    pub id: String,
    pub r#type: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum Stop {
    Single(String),
    Multiple(Vec<String>),
}

// ============================================================================
// Conversion Functions
// ============================================================================

/// Flatten Responses-API tool specs into the flat `function` definitions that
/// Chat Completions requires. Namespace containers (MCP servers and other
/// multi-tool sources) are expanded into individual functions named
/// `<namespace><tool>` (e.g. `mcp__context7__resolve-library-id`) so the model
/// can call each tool and the registry can route the flat name back to the
/// handler. Non-namespace tools pass through unchanged.
fn flatten_tools_for_chat(tools: &[Value]) -> Vec<Value> {
    let mut out = Vec::with_capacity(tools.len());
    for tool in tools {
        if tool.get("type").and_then(|v| v.as_str()) == Some("namespace") {
            let namespace = tool.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if let Some(nested) = tool.get("tools").and_then(|v| v.as_array()) {
                for n in nested {
                    if let Some(flat) = flatten_namespaced_function(n, namespace) {
                        out.push(flat);
                    }
                }
                continue;
            }
        }
        out.push(tool.clone());
    }
    out
}

/// Build a flat function definition from a single nested tool inside a
/// namespace container. Returns `None` for non-function entries.
fn flatten_namespaced_function(nested: &Value, namespace: &str) -> Option<Value> {
    if nested.get("type").and_then(|v| v.as_str()) != Some("function") {
        return None;
    }
    let name = nested.get("name")?.as_str()?;
    let flat_name = format!("{namespace}{name}");
    let description = nested
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let parameters = nested
        .get("parameters")
        .cloned()
        .unwrap_or_else(|| json!({}));
    Some(json!({
        "type": "function",
        "name": flat_name,
        "description": description,
        "parameters": parameters,
    }))
}

/// Convert internal protocol request to Chat Completions API request.
///
/// `reasoning.summary` has no Chat Completions equivalent (it is a Responses-API
/// concept) and is intentionally not serialized here; only `reasoning.effort` is
/// forwarded as the top-level `reasoning_effort` field.
#[allow(clippy::too_many_arguments)]
pub fn to_chat_completions_request(
    model: &str,
    input: &[ResponseItem],
    instructions: &str,
    tools: &[Value],
    tool_choice: &str,
    parallel_tool_calls: bool,
    reasoning: Option<Reasoning>,
    service_tier: Option<&str>,
    max_completion_tokens: Option<u32>,
) -> ChatCompletionRequest {
    let mut messages = Vec::new();

    // Add system instructions first if present
    if !instructions.is_empty() {
        messages.push(ChatMessage {
            role: "system".to_string(),
            content: Some(instructions.to_string()),
            tool_call_id: None,
            tool_calls: None,
        });
    }

    // Convert input ResponseItems to ChatMessages
    for item in input {
        match item {
            ResponseItem::Message { role, content, .. } => {
                // Convert ContentItem vector to a simple string
                let text_content = content
                    .iter()
                    .filter_map(|item| match item {
                        ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                            Some(text.clone())
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");

                messages.push(ChatMessage {
                    role: role.clone(),
                    content: if text_content.is_empty() {
                        None
                    } else {
                        Some(text_content)
                    },
                    tool_call_id: None,
                    tool_calls: None,
                });
            }
            // Assistant tool invocation (Responses-API built-in tool call).
            // `tool_call_id` is only valid on role:"tool" result messages, so it
            // must be `None` here; the call id lives in `tool_calls[0].id`.
            ResponseItem::FunctionCall {
                name,
                arguments,
                call_id,
                ..
            } => {
                messages.push(assistant_tool_call_message(call_id, name, arguments));
            }
            // Assistant tool invocation (custom/freeform tool call). The arguments
            // are carried in the `input` field in the internal protocol.
            ResponseItem::CustomToolCall {
                name,
                input,
                call_id,
                ..
            } => {
                messages.push(assistant_tool_call_message(call_id, name, input));
            }
            // Tool results MUST be sent as role:"tool" messages with the matching
            // `tool_call_id`, otherwise the model never sees the outcome of the
            // tool it invoked and multi-turn tool calling is broken.
            ResponseItem::FunctionCallOutput {
                call_id, output, ..
            }
            | ResponseItem::CustomToolCallOutput {
                call_id, output, ..
            } => {
                let content = output
                    .text_content()
                    .map(str::to_string)
                    .unwrap_or_else(|| serde_json::to_string(&output).unwrap_or_default());
                messages.push(ChatMessage {
                    role: "tool".to_string(),
                    content: if content.is_empty() {
                        None
                    } else {
                        Some(content)
                    },
                    tool_call_id: Some(call_id.clone()),
                    tool_calls: None,
                });
            }
            // Other item types (reasoning, local-shell, web/image/tool-search
            // calls, ...) have no Chat Completions representation and are omitted.
            _ => {}
        }
    }

    // Convert tools. Namespace containers (MCP servers and other multi-tool
    // sources) must be flattened first: Chat Completions only supports flat
    // `function` definitions, so each nested tool becomes its own function
    // named `<namespace><tool>` (e.g. `mcp__context7__resolve-library-id`). The
    // registry resolves that flat name back to the handler.
    let chat_tools: Vec<ChatTool> = flatten_tools_for_chat(tools)
        .iter()
        .map(tool_to_function_definition)
        .collect();

    ChatCompletionRequest {
        model: model.to_string(),
        messages,
        tools: chat_tools,
        tool_choice: if tool_choice == "none" || tool_choice == "auto" || tool_choice == "required"
        {
            Some(tool_choice.to_string())
        } else {
            None
        },
        parallel_tool_calls: Some(parallel_tool_calls),
        max_tokens: None,
        max_completion_tokens,
        temperature: None,
        top_p: None,
        n: None,
        stream: Some(true),
        stream_options: Some(StreamOptions {
            include_usage: true,
        }),
        stop: None,
        reasoning_effort: reasoning.and_then(|r| r.effort),
        service_tier: service_tier.map(str::to_string),
    }
}

/// Builds an assistant `ChatMessage` carrying a single tool call. Shared by the
/// `FunctionCall` and `CustomToolCall` conversions.
fn assistant_tool_call_message(call_id: &str, name: &str, arguments: &str) -> ChatMessage {
    ChatMessage {
        role: "assistant".to_string(),
        content: None,
        tool_call_id: None,
        tool_calls: Some(vec![ChatMessageToolCall {
            id: call_id.to_string(),
            r#type: "function".to_string(),
            function: FunctionCall {
                name: name.to_string(),
                arguments: arguments.to_string(),
            },
        }]),
    }
}

/// Convert internal tool definition to Chat Completions function definition
pub fn tool_to_function_definition(tool: &Value) -> ChatTool {
    // Handle nested tool structure: { type: "function", function: { ... } }
    let (name, description, parameters) =
        if tool.get("type").and_then(|v| v.as_str()) == Some("function") {
            // Nested structure
            let function = tool.get("function").unwrap_or(tool);
            (
                function
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                {
                    let mut desc = function
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    // Handle namespace in description
                    if let Some(namespace) = function.get("namespace").and_then(|v| v.as_str())
                        && !desc.is_empty()
                    {
                        desc = format!("[{namespace}] {desc}");
                    }
                    desc
                },
                function.get("parameters").cloned().unwrap_or(json!({})),
            )
        } else {
            // Flat structure
            let name = tool
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let mut description = tool
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            // Handle namespace in description
            if let Some(namespace) = tool.get("namespace").and_then(|v| v.as_str())
                && !description.is_empty()
            {
                description = format!("[{namespace}] {description}");
            }

            let parameters = tool.get("parameters").cloned().unwrap_or(json!({}));
            (name, description, parameters)
        };

    ChatTool {
        r#type: "function".to_string(),
        function: FunctionDefinition {
            name,
            description,
            parameters,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::Reasoning;
    use codex_protocol::models::ContentItem;
    use codex_protocol::models::FunctionCallOutputPayload;
    use codex_protocol::models::ResponseItem;
    use codex_protocol::openai_models::ReasoningEffort;
    use serde_json::json;

    #[test]
    fn test_chat_completion_request_serialization() {
        let request = ChatCompletionRequest {
            model: "gpt-4".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: Some("Hello".to_string()),
                tool_call_id: None,
                tool_calls: None,
            }],
            tools: vec![],
            tool_choice: Some("auto".to_string()),
            parallel_tool_calls: Some(true),
            max_tokens: None,
            max_completion_tokens: None,
            temperature: None,
            top_p: None,
            n: None,
            stream: Some(true),
            stream_options: Some(StreamOptions {
                include_usage: true,
            }),
            stop: None,
            reasoning_effort: None,
            service_tier: None,
        };

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("gpt-4"));
        assert!(json.contains("user"));
        assert!(json.contains("Hello"));
        assert!(json.contains("parallel_tool_calls"));
        assert!(json.contains("\"stream\":true"));
        assert!(json.contains("include_usage"));
    }

    #[test]
    fn test_convert_request_with_messages() {
        let input = vec![
            ResponseItem::Message {
                id: Some("msg1".to_string()),
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "Hello, how are you?".to_string(),
                }],
                phase: None,
                internal_chat_message_metadata_passthrough: None,
            },
            ResponseItem::Message {
                id: Some("msg2".to_string()),
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText {
                    text: "I'm doing well!".to_string(),
                }],
                phase: None,
                internal_chat_message_metadata_passthrough: None,
            },
        ];

        let request = to_chat_completions_request(
            "gpt-4",
            &input,
            "You are a helpful assistant.",
            &[],
            "auto",
            true,
            None,
            None,
            None,
        );

        assert_eq!(request.model, "gpt-4");
        assert_eq!(request.messages.len(), 3); // system + 2 messages
        assert_eq!(request.messages[0].role, "system");
        assert_eq!(request.messages[1].role, "user");
        assert_eq!(
            request.messages[1].content,
            Some("Hello, how are you?".to_string())
        );
        assert_eq!(request.messages[2].role, "assistant");
        assert_eq!(
            request.messages[2].content,
            Some("I'm doing well!".to_string())
        );
    }

    #[test]
    fn test_convert_request_with_tools() {
        let input = vec![ResponseItem::Message {
            id: Some("msg1".to_string()),
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "What's the weather?".to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        }];

        let tools = vec![json!({
            "type": "function",
            "function": {
                "name": "get_weather",
                "description": "Get current weather",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "location": {
                            "type": "string",
                            "description": "City name"
                        }
                    },
                    "required": ["location"]
                }
            }
        })];

        let request = to_chat_completions_request(
            "gpt-4", &input, "", &tools, "auto", true, None, None, None,
        );

        assert_eq!(request.tools.len(), 1);
        assert_eq!(request.tools[0].r#type, "function");
        assert_eq!(request.tools[0].function.name, "get_weather");
    }

    #[test]
    fn test_convert_request_parallel_tool_calls_enabled() {
        let input = vec![ResponseItem::Message {
            id: Some("msg1".to_string()),
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "Check weather and time".to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        }];

        let request = to_chat_completions_request(
            "gpt-4",
            &input,
            "",
            &[],
            "auto",
            true, // parallel_tool_calls enabled
            None,
            None,
            None,
        );

        assert_eq!(request.parallel_tool_calls, Some(true));
    }

    #[test]
    fn test_convert_request_parallel_tool_calls_disabled() {
        let input = vec![ResponseItem::Message {
            id: Some("msg1".to_string()),
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "Check weather".to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        }];

        let request = to_chat_completions_request(
            "gpt-4",
            &input,
            "",
            &[],
            "auto",
            false, // parallel_tool_calls disabled
            None,
            None,
            None,
        );

        // `false` must be forwarded as Some(false) so the provider actually
        // disables parallel tool calls; the old `then_some(true)` collapsed it
        // to None, which providers treat as the default (enabled).
        assert_eq!(request.parallel_tool_calls, Some(false));
    }

    #[test]
    fn test_convert_request_with_system_instructions() {
        let input = vec![ResponseItem::Message {
            id: Some("msg1".to_string()),
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "Hello".to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        }];

        let request = to_chat_completions_request(
            "gpt-4",
            &input,
            "You are a helpful assistant with expertise in Rust.",
            &[],
            "auto",
            true,
            None,
            None,
            None,
        );

        // System instructions should be first message
        assert_eq!(request.messages.len(), 2);
        assert_eq!(request.messages[0].role, "system");
        assert_eq!(
            request.messages[0].content,
            Some("You are a helpful assistant with expertise in Rust.".to_string())
        );
        assert_eq!(request.messages[1].role, "user");
        assert_eq!(request.messages[1].content, Some("Hello".to_string()));
    }

    #[test]
    fn test_convert_empty_request() {
        let input = vec![];

        let request =
            to_chat_completions_request("gpt-4", &input, "", &[], "auto", true, None, None, None);

        assert_eq!(request.model, "gpt-4");
        assert_eq!(request.messages.len(), 0);
    }

    #[test]
    fn test_convert_request_with_reasoning() {
        let input = vec![ResponseItem::Message {
            id: Some("msg1".to_string()),
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "Solve this problem".to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        }];

        let reasoning = Reasoning {
            effort: Some(ReasoningEffort::Medium),
            summary: None,
            context: None,
        };

        let request = to_chat_completions_request(
            "o1-preview",
            &input,
            "",
            &[],
            "auto",
            true,
            Some(reasoning),
            None,
            None,
        );

        // Reasoning effort must be propagated, not silently dropped.
        assert_eq!(request.reasoning_effort, Some(ReasoningEffort::Medium));
        let serialized = serde_json::to_string(&request).unwrap();
        assert!(serialized.contains("\"reasoning_effort\":\"medium\""));
    }

    #[test]
    fn test_convert_request_with_service_tier_and_budget() {
        let input = vec![ResponseItem::Message {
            id: Some("msg1".to_string()),
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "hi".to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        }];

        let request = to_chat_completions_request(
            "gpt-4",
            &input,
            "",
            &[],
            "auto",
            true,
            None,
            Some("flex"),
            Some(1024),
        );

        assert_eq!(request.service_tier.as_deref(), Some("flex"));
        assert_eq!(request.max_completion_tokens, Some(1024));
    }

    #[test]
    fn test_function_call_maps_to_assistant_tool_call() {
        let input = vec![ResponseItem::FunctionCall {
            id: Some("fc_1".to_string()),
            name: "get_weather".to_string(),
            namespace: None,
            arguments: r#"{"location":"SF"}"#.to_string(),
            call_id: "call_abc".to_string(),
            internal_chat_message_metadata_passthrough: None,
        }];

        let request =
            to_chat_completions_request("gpt-4", &input, "", &[], "auto", true, None, None, None);

        assert_eq!(request.messages.len(), 1);
        let msg = &request.messages[0];
        assert_eq!(msg.role, "assistant");
        // tool_call_id is invalid on assistant messages (spec-forbidden).
        assert_eq!(msg.tool_call_id, None);
        let tool_calls = msg.tool_calls.as_ref().expect("tool_calls present");
        assert_eq!(tool_calls.len(), 1);
        // Correlation id must be the call_id, not the legacy optional id.
        assert_eq!(tool_calls[0].id, "call_abc");
        assert_eq!(tool_calls[0].function.name, "get_weather");
        assert_eq!(tool_calls[0].function.arguments, r#"{"location":"SF"}"#);
    }

    #[test]
    fn test_custom_tool_call_maps_to_assistant_tool_call() {
        let input = vec![ResponseItem::CustomToolCall {
            id: Some("c_1".to_string()),
            status: None,
            call_id: "call_xyz".to_string(),
            name: "run_shell".to_string(),
            namespace: None,
            input: "{\"cmd\":\"ls\"}".to_string(),
            internal_chat_message_metadata_passthrough: None,
        }];

        let request =
            to_chat_completions_request("gpt-4", &input, "", &[], "auto", true, None, None, None);

        let msg = &request.messages[0];
        assert_eq!(msg.role, "assistant");
        let tc = msg.tool_calls.as_ref().unwrap().first().unwrap();
        assert_eq!(tc.id, "call_xyz");
        assert_eq!(tc.function.name, "run_shell");
        assert_eq!(tc.function.arguments, "{\"cmd\":\"ls\"}");
    }

    #[test]
    fn test_tool_result_maps_to_tool_role_message() {
        let input = vec![ResponseItem::FunctionCallOutput {
            id: None,
            call_id: "call_abc".to_string(),
            output: FunctionCallOutputPayload::from_text("72F, sunny".to_string()),
            internal_chat_message_metadata_passthrough: None,
        }];

        let request =
            to_chat_completions_request("gpt-4", &input, "", &[], "auto", true, None, None, None);

        assert_eq!(request.messages.len(), 1);
        let msg = &request.messages[0];
        assert_eq!(msg.role, "tool");
        assert_eq!(msg.tool_call_id.as_deref(), Some("call_abc"));
        assert_eq!(msg.content.as_deref(), Some("72F, sunny"));
        assert!(msg.tool_calls.is_none());
    }

    #[test]
    fn test_custom_tool_result_maps_to_tool_role_message() {
        let input = vec![ResponseItem::CustomToolCallOutput {
            id: None,
            call_id: "call_xyz".to_string(),
            name: Some("run_shell".to_string()),
            output: FunctionCallOutputPayload::from_text("done".to_string()),
            internal_chat_message_metadata_passthrough: None,
        }];

        let request =
            to_chat_completions_request("gpt-4", &input, "", &[], "auto", true, None, None, None);

        let msg = &request.messages[0];
        assert_eq!(msg.role, "tool");
        assert_eq!(msg.tool_call_id.as_deref(), Some("call_xyz"));
    }

    #[test]
    fn test_tool_definition_from_internal_tool() {
        let internal_tool = json!({
            "name": "search_files",
            "namespace": "filesystem",
            "description": "Search for files",
            "parameters": {
                "type": "object",
                "properties": {
                    "pattern": {"type": "string"},
                    "path": {"type": "string"}
                },
                "required": ["pattern"]
            }
        });

        let tool = tool_to_function_definition(&internal_tool);

        assert_eq!(tool.function.name, "search_files");
        assert_eq!(tool.function.description, "[filesystem] Search for files");
        // Check that parameters is an object with properties
        if let serde_json::Value::Object(map) = &tool.function.parameters {
            assert!(map.contains_key("properties"));
        }
    }

    #[test]
    fn test_tool_definition_with_namespace() {
        let internal_tool = json!({
            "name": "read_file",
            "namespace": "fs",
            "description": "Read a file",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                },
                "required": ["path"]
            }
        });

        let tool = tool_to_function_definition(&internal_tool);

        // Namespace is prefixed into the description; name is unmodified.
        assert_eq!(tool.function.name, "read_file");
        assert_eq!(tool.function.description, "[fs] Read a file");
    }

    #[test]
    fn flatten_namespace_tools_into_flat_functions() {
        // A namespace container (as produced for an MCP server) must expand into
        // individual flat functions named `<namespace><tool>` for Chat Completions.
        let namespace = json!({
            "type": "namespace",
            "name": "mcp__demo__",
            "description": "Demo tools",
            "tools": [
                {
                    "type": "function",
                    "name": "lookup_order",
                    "description": "Look up an order",
                    "parameters": {"type": "object"}
                },
                {
                    "type": "function",
                    "name": "cancel_order",
                    "description": "Cancel an order",
                    "parameters": {"type": "object"}
                }
            ]
        });
        let flat = json!({
            "type": "function",
            "name": "exec_command",
            "description": "Run a command",
            "parameters": {"type": "object"}
        });

        let out = flatten_tools_for_chat(&[namespace, flat]);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0]["name"].as_str(), Some("mcp__demo__lookup_order"));
        assert_eq!(out[1]["name"].as_str(), Some("mcp__demo__cancel_order"));
        assert_eq!(out[2]["name"].as_str(), Some("exec_command"));

        // Each flattened entry must round-trip through tool_to_function_definition.
        let chat_tool = tool_to_function_definition(&out[0]);
        assert_eq!(chat_tool.function.name, "mcp__demo__lookup_order");
    }
}
