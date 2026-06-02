//! Chat Completions API response conversion to internal protocol format.
//!
//! This module handles converting Chat Completions API responses and SSE events
//! into the internal ResponseEvent format used by Codex.

use crate::common::ResponseEvent;
use crate::error::ApiError;
use codex_protocol::protocol::TokenUsage;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;

// ============================================================================
// Chat Completions API Types
// ============================================================================

/// Chat Completions API response (non-streaming)
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ChatCompletionResponse {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<ChatChoice>,
    pub usage: Option<ChatUsage>,
}

#[derive(Debug, Deserialize)]
pub struct ChatChoice {
    pub index: usize,
    pub message: ChatMessage,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: Option<String>,
    pub tool_calls: Option<Vec<ChatToolCall>>,
}

#[derive(Debug, Deserialize)]
pub struct ChatToolCall {
    pub id: String,
    pub r#type: Option<String>,
    pub function: ChatFunctionCall,
}

#[derive(Debug, Deserialize)]
pub struct ChatFunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Deserialize)]
pub struct ChatUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

/// Chat Completions streaming chunk
#[derive(Debug, Deserialize)]
pub struct ChatCompletionChunk {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<ChatChoiceDelta>,
}

#[derive(Debug, Deserialize)]
pub struct ChatChoiceDelta {
    pub index: usize,
    pub delta: ChatDelta,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ChatDelta {
    pub role: Option<String>,
    pub content: Option<String>,
    pub tool_calls: Option<Vec<ChatToolCallDelta>>,
}

#[derive(Debug, Deserialize)]
pub struct ChatToolCallDelta {
    pub index: usize,
    pub id: Option<String>,
    pub r#type: Option<String>,
    pub function: Option<ChatFunctionCallDelta>,
}

#[derive(Debug, Deserialize)]
pub struct ChatFunctionCallDelta {
    pub name: Option<String>,
    pub arguments: Option<String>,
}

/// Chat Completions API error response
#[derive(Debug, Deserialize)]
pub struct ChatError {
    pub error: ChatErrorDetail,
}

#[derive(Debug, Deserialize)]
pub struct ChatErrorDetail {
    pub message: String,
    pub r#type: String,
    pub code: Option<String>,
}

// ============================================================================
// Conversion Functions
// ============================================================================

/// Converts a Chat Completions response to internal ResponseEvent format
pub fn convert_chat_response_to_internal(
    response: ChatCompletionResponse,
) -> Vec<ResponseEvent> {
    let mut events = Vec::new();

    // Emit Created event
    events.push(ResponseEvent::Created);

    // Emit text content as OutputTextDelta events
    for choice in &response.choices {
        if let Some(content) = &choice.message.content {
            for chunk in content.split_bytes(100) {
                events.push(ResponseEvent::OutputTextDelta(
                    String::from_utf8_lossy(&chunk).to_string(),
                ));
            }
        }
    }

    // Emit tool calls if present
    for choice in &response.choices {
        if let Some(tool_calls) = &choice.message.tool_calls {
            for tool_call in tool_calls {
                events.push(ResponseEvent::ToolCallInputDelta {
                    item_id: tool_call.id.clone(),
                    call_id: Some(tool_call.id.clone()),
                    delta: tool_call.function.arguments.clone(),
                });
            }
        }
    }

    // Emit Completed event with usage
    let token_usage = response.usage.map(|usage| TokenUsage {
        input_tokens: usage.prompt_tokens as i64,
        cached_input_tokens: 0,
        output_tokens: usage.completion_tokens as i64,
        reasoning_output_tokens: 0,
        total_tokens: usage.total_tokens as i64,
    });

    events.push(ResponseEvent::Completed {
        response_id: response.id,
        token_usage,
        end_turn: Some(true),
    });

    events
}

/// Converts a tool call from Chat Completions response to internal format
pub fn convert_tool_call_from_chat_response(
    tool_call: ChatToolCall,
) -> ResponseEvent {
    ResponseEvent::ToolCallInputDelta {
        item_id: tool_call.id.clone(),
        call_id: Some(tool_call.id),
        delta: tool_call.function.arguments,
    }
}

/// Converts Chat Completions SSE chunk to internal ResponseEvent
pub fn convert_chat_sse_chunk_to_event(
    chunk: ChatCompletionChunk,
) -> Option<ResponseEvent> {
    // Check for [DONE] sentinel (stream termination)
    if chunk.choices.is_empty() {
        return None;
    }

    for choice in chunk.choices {
        // Handle text delta
        if let Some(content) = choice.delta.content {
            return Some(ResponseEvent::OutputTextDelta(content));
        }

        // Handle tool call delta
        if let Some(tool_calls) = choice.delta.tool_calls {
            for tool_call in tool_calls {
                if let Some(args) = tool_call.function.and_then(|f| f.arguments) {
                    return Some(ResponseEvent::ToolCallInputDelta {
                        item_id: format!("tool_{}", tool_call.index),
                        call_id: tool_call.id,
                        delta: args,
                    });
                }
            }
        }

        // Handle completion
        if let Some(finish_reason) = choice.finish_reason {
            if finish_reason == "stop" {
                return Some(ResponseEvent::Completed {
                    response_id: chunk.id.clone(),
                    token_usage: None,
                    end_turn: Some(true),
                });
            }
        }
    }

    None
}

/// Maps Chat Completions API error to internal ApiError
pub fn map_chat_error_to_api_error(error: ChatError) -> ApiError {
    match error.error.r#type.as_str() {
        "invalid_request_error" => ApiError::Api {
            status: http::StatusCode::BAD_REQUEST,
            message: error.error.message,
        },
        "authentication_error" => ApiError::Api {
            status: http::StatusCode::UNAUTHORIZED,
            message: error.error.message,
        },
        "rate_limit_error" => ApiError::RateLimit(error.error.message),
        "context_length_exceeded" => ApiError::ContextWindowExceeded,
        "server_error" => ApiError::Api {
            status: http::StatusCode::INTERNAL_SERVER_ERROR,
            message: error.error.message,
        },
        _ => ApiError::Api {
            status: http::StatusCode::INTERNAL_SERVER_ERROR,
            message: error.error.message,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::StatusCode;
    use serde_json::json;

    #[test]
    fn test_convert_chat_response_to_internal() {
        let response = ChatCompletionResponse {
            id: "chatcmpl-123".to_string(),
            object: "chat.completion".to_string(),
            created: 1677652288,
            model: "gpt-5".to_string(),
            choices: vec![ChatChoice {
                index: 0,
                message: ChatMessage {
                    role: "assistant".to_string(),
                    content: Some("Hello, world!".to_string()),
                    tool_calls: None,
                },
                finish_reason: Some("stop".to_string()),
            }],
            usage: Some(ChatUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            }),
        };

        let events = convert_chat_response_to_internal(response);

        assert_eq!(events.len(), 3); // Created, OutputTextDelta, Completed
        assert!(matches!(events[0], ResponseEvent::Created));
        assert!(matches!(events[1], ResponseEvent::OutputTextDelta(_)));
        assert!(matches!(events[2], ResponseEvent::Completed { .. }));
    }

    #[test]
    fn test_convert_tool_call_from_chat_response() {
        let tool_call = ChatToolCall {
            id: "call_abc123".to_string(),
            r#type: Some("function".to_string()),
            function: ChatFunctionCall {
                name: "search".to_string(),
                arguments: r#"{"query":"test"}"#.to_string(),
            },
        };

        let event = convert_tool_call_from_chat_response(tool_call);

        match event {
            ResponseEvent::ToolCallInputDelta {
                item_id,
                call_id,
                delta,
            } => {
                assert_eq!(item_id, "call_abc123");
                assert_eq!(call_id, Some("call_abc123".to_string()));
                assert_eq!(delta, r#"{"query":"test"}"#);
            }
            _ => panic!("Expected ToolCallInputDelta event"),
        }
    }

    #[test]
    fn test_sse_text_delta_emits_per_chunk() {
        let chunk = ChatCompletionChunk {
            id: "chatcmpl-123".to_string(),
            object: "chat.completion.chunk".to_string(),
            created: 1677652288,
            model: "gpt-5".to_string(),
            choices: vec![ChatChoiceDelta {
                index: 0,
                delta: ChatDelta {
                    role: None,
                    content: Some("Hello".to_string()),
                    tool_calls: None,
                },
                finish_reason: None,
            }],
        };

        let event = convert_chat_sse_chunk_to_event(chunk);

        match event {
            Some(ResponseEvent::OutputTextDelta(text)) => {
                assert_eq!(text, "Hello");
            }
            _ => panic!("Expected OutputTextDelta event"),
        }
    }

    #[test]
    fn test_sse_tool_call_delta_emits_buffered() {
        let chunk = ChatCompletionChunk {
            id: "chatcmpl-123".to_string(),
            object: "chat.completion.chunk".to_string(),
            created: 1677652288,
            model: "gpt-5".to_string(),
            choices: vec![ChatChoiceDelta {
                index: 0,
                delta: ChatDelta {
                    role: None,
                    content: None,
                    tool_calls: Some(vec![ChatToolCallDelta {
                        index: 0,
                        id: Some("call_123".to_string()),
                        r#type: Some("function".to_string()),
                        function: Some(ChatFunctionCallDelta {
                            name: None,
                            arguments: Some(r#"{"query":"test"}"#.to_string()),
                        }),
                    }]),
                },
                finish_reason: None,
            }],
        };

        let event = convert_chat_sse_chunk_to_event(chunk);

        match event {
            Some(ResponseEvent::ToolCallInputDelta {
                item_id,
                call_id,
                delta,
            }) => {
                assert_eq!(item_id, "tool_0");
                assert_eq!(call_id, Some("call_123".to_string()));
                assert_eq!(delta, r#"{"query":"test"}"#);
            }
            _ => panic!("Expected ToolCallInputDelta event"),
        }
    }

    #[test]
    fn test_error_response_maps_invalid_request_to_api_error() {
        let error = ChatError {
            error: ChatErrorDetail {
                message: "Invalid request".to_string(),
                r#type: "invalid_request_error".to_string(),
                code: Some("invalid_request".to_string()),
            },
        };

        let api_error = map_chat_error_to_api_error(error);

        match api_error {
            ApiError::Api { status, message } => {
                assert_eq!(status, StatusCode::BAD_REQUEST);
                assert_eq!(message, "Invalid request");
            }
            _ => panic!("Expected ApiError::Api with BAD_REQUEST status"),
        }
    }

    #[test]
    fn test_rate_limit_error_maps_correctly() {
        let error = ChatError {
            error: ChatErrorDetail {
                message: "Rate limit exceeded".to_string(),
                r#type: "rate_limit_error".to_string(),
                code: Some("rate_limit_exceeded".to_string()),
            },
        };

        let api_error = map_chat_error_to_api_error(error);

        match api_error {
            ApiError::RateLimit(message) => {
                assert_eq!(message, "Rate limit exceeded");
            }
            _ => panic!("Expected ApiError::RateLimit"),
        }
    }

    #[test]
    fn test_context_length_exceeded_error_maps_correctly() {
        let error = ChatError {
            error: ChatErrorDetail {
                message: "Context length exceeded".to_string(),
                r#type: "context_length_exceeded".to_string(),
                code: None,
            },
        };

        let api_error = map_chat_error_to_api_error(error);

        match api_error {
            ApiError::ContextWindowExceeded => {
                // Expected
            }
            _ => panic!("Expected ApiError::ContextWindowExceeded"),
        }
    }

    #[test]
    fn test_authentication_error_maps_to_401() {
        let error = ChatError {
            error: ChatErrorDetail {
                message: "Invalid API key".to_string(),
                r#type: "authentication_error".to_string(),
                code: Some("invalid_api_key".to_string()),
            },
        };

        let api_error = map_chat_error_to_api_error(error);

        match api_error {
            ApiError::Api { status, message } => {
                assert_eq!(status, StatusCode::UNAUTHORIZED);
                assert_eq!(message, "Invalid API key");
            }
            _ => panic!("Expected ApiError::Api with UNAUTHORIZED status"),
        }
    }

    #[test]
    fn test_server_error_maps_to_500() {
        let error = ChatError {
            error: ChatErrorDetail {
                message: "Internal server error".to_string(),
                r#type: "server_error".to_string(),
                code: None,
            },
        };

        let api_error = map_chat_error_to_api_error(error);

        match api_error {
            ApiError::Api { status, message } => {
                assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
                assert_eq!(message, "Internal server error");
            }
            _ => panic!("Expected ApiError::Api with INTERNAL_SERVER_ERROR status"),
        }
    }
}

// Helper trait for splitting content into chunks
trait SplitChunks {
    fn split_bytes(&self, chunk_size: usize) -> Vec<Vec<u8>>;
}

impl SplitChunks for String {
    fn split_bytes(&self, chunk_size: usize) -> Vec<Vec<u8>> {
        self.as_bytes()
            .chunks(chunk_size)
            .map(|chunk| chunk.to_vec())
            .collect()
    }
}
