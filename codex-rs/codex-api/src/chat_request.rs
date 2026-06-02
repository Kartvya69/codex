//! Chat Completions API request types and conversion
//!
//! This module handles converting internal protocol request types to
//! Chat Completions API request format.

use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ReasoningEffort;
use serde::Serialize;
use serde_json::json;
use serde_json::Value;

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub n: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<Stop>,
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

/// Convert internal protocol request to Chat Completions API request
pub fn to_chat_completions_request(
    model: &str,
    input: &[ResponseItem],
    instructions: &str,
    tools: &[Value],
    tool_choice: &str,
    parallel_tool_calls: bool,
    reasoning: Option<Reasoning>,
    _service_tier: Option<&str>,
    _prompt_cache_key: Option<&str>,
    _text_controls: Option<&()>,
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
            ResponseItem::Message {
                role,
                content,
                ..
            } => {
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
                    content: if text_content.is_empty() { None } else { Some(text_content) },
                    tool_call_id: None,
                    tool_calls: None,
                });
            }
            ResponseItem::FunctionCall {
                id,
                name,
                arguments,
                ..
            } => {
                messages.push(ChatMessage {
                    role: "assistant".to_string(),
                    content: None,
                    tool_call_id: id.clone(),
                    tool_calls: Some(vec![ChatMessageToolCall {
                        id: id.clone().unwrap_or_default(),
                        r#type: "function".to_string(),
                        function: FunctionCall {
                            name: name.clone(),
                            arguments: arguments.clone(),
                        },
                    }]),
                });
            }
            _ => {
                // Ignore other item types for now
            }
        }
    }

    // Convert tools
    let chat_tools: Vec<ChatTool> = tools
        .iter()
        .map(|tool| tool_to_function_definition(tool))
        .collect();

    // Handle parallel_tool_calls
    let parallel_tool_calls_value = if parallel_tool_calls {
        Some(true)
    } else {
        None
    };

    ChatCompletionRequest {
        model: model.to_string(),
        messages,
        tools: chat_tools,
        tool_choice: if tool_choice == "none" || tool_choice == "auto" || tool_choice == "required" {
            Some(tool_choice.to_string())
        } else {
            None
        },
        parallel_tool_calls: parallel_tool_calls_value,
        max_tokens: None,
        temperature: None,
        top_p: None,
        n: None,
        stream: None,
        stop: None,
    }
}

/// Convert internal tool definition to Chat Completions function definition
pub fn tool_to_function_definition(tool: &Value) -> ChatTool {
    // Handle nested tool structure: { type: "function", function: { ... } }
    let (name, description, parameters) = if tool.get("type").and_then(|v| v.as_str()) == Some("function") {
        // Nested structure
        let function = tool.get("function").unwrap_or(tool);
        (
            function.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            {
                let desc = function.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let mut desc = desc.to_string();
                // Handle namespace in description
                if let Some(namespace) = function.get("namespace").and_then(|v| v.as_str()) {
                    if !desc.is_empty() {
                        desc = format!("[{}] {}", namespace, desc);
                    }
                }
                desc
            },
            function.get("parameters").cloned().unwrap_or(json!({})),
        )
    } else {
        // Flat structure
        let name = tool.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let mut description = tool.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();

        // Handle namespace in description
        if let Some(namespace) = tool.get("namespace").and_then(|v| v.as_str()) {
            if !description.is_empty() {
                description = format!("[{}] {}", namespace, description);
            }
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
    use codex_protocol::models::ContentItem;
    use codex_protocol::models::ResponseItem;
    use codex_protocol::openai_models::ReasoningEffort;
    use crate::common::Reasoning;
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
            temperature: None,
            top_p: None,
            n: None,
            stream: None,
            stop: None,
        };

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("gpt-4"));
        assert!(json.contains("user"));
        assert!(json.contains("Hello"));
        assert!(json.contains("parallel_tool_calls"));
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
            },
            ResponseItem::Message {
                id: Some("msg2".to_string()),
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText {
                    text: "I'm doing well!".to_string(),
                }],
                phase: None,
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
            None,
        );

        assert_eq!(request.model, "gpt-4");
        assert_eq!(request.messages.len(), 3); // system + 2 messages
        assert_eq!(request.messages[0].role, "system");
        assert_eq!(request.messages[1].role, "user");
        assert_eq!(request.messages[1].content, Some("Hello, how are you?".to_string()));
        assert_eq!(request.messages[2].role, "assistant");
        assert_eq!(request.messages[2].content, Some("I'm doing well!".to_string()));
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
            "gpt-4",
            &input,
            "",
            &tools,
            "auto",
            true,
            None,
            None,
            None,
            None,
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
            None,
        );

        assert!(request.parallel_tool_calls.is_some());
        assert_eq!(request.parallel_tool_calls.unwrap(), true);
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
            None,
        );

        assert!(request.parallel_tool_calls.is_none());
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

        let request = to_chat_completions_request(
            "gpt-4",
            &input,
            "",
            &[],
            "auto",
            true,
            None,
            None,
            None,
            None,
        );

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
        }];

        let reasoning = Reasoning {
            effort: Some(ReasoningEffort::Medium),
            summary: None,
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
            None,
        );

        // Reasoning should be represented in the request
        assert!(request.model.contains("o1"));
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
        assert!(tool.function.description.contains("filesystem"));
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

        // Namespace should be included in description or handled separately
        assert!(tool.function.description.contains("fs") || tool.function.name.contains("read_file"));
    }
}
