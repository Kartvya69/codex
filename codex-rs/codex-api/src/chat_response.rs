//! Chat Completions API response types and error mapping.
//!
//! The Chat Completions wire path used by codex-api is streaming-only: the
//! request always carries `stream: true` and the transport consumes an SSE
//! stream. Accordingly the streaming event synthesis (chunk -> `ResponseEvent`,
//! including the stateful tool-call item lifecycle) lives in
//! [`crate::sse::chat`]. This module owns the wire deserialization types and the
//! Chat-Completions error -> [`ApiError`] mapping.

use crate::error::ApiError;
use serde::Deserialize;

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

#[derive(Debug, Clone, Deserialize)]
pub struct ChatUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

/// Chat Completions streaming chunk.
///
/// When `stream_options.include_usage` is set, the final chunk (after the
/// `finish_reason` chunk) carries an empty `choices` array and a populated
/// [`ChatUsage`].
#[derive(Debug, Deserialize)]
pub struct ChatCompletionChunk {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    #[serde(default)]
    pub choices: Vec<ChatChoiceDelta>,
    #[serde(default)]
    pub usage: Option<ChatUsage>,
}

#[derive(Debug, Deserialize)]
pub struct ChatChoiceDelta {
    pub index: usize,
    pub delta: ChatDelta,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ChatDelta {
    pub role: Option<String>,
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<ChatToolCallDelta>>,
}

#[derive(Debug, Deserialize)]
pub struct ChatToolCallDelta {
    pub index: usize,
    pub id: Option<String>,
    pub r#type: Option<String>,
    pub function: Option<ChatFunctionCallDelta>,
}

#[derive(Debug, Default, Deserialize)]
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
// Error Mapping
// ============================================================================

/// Maps a Chat Completions API error object to an internal [`ApiError`].
///
/// This is invoked from the SSE loop when a stream data frame fails to parse as
/// a [`ChatCompletionChunk`] but successfully parses as a [`ChatError`], so
/// provider errors delivered mid-stream are surfaced instead of silently
/// swallowed.
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

        assert!(matches!(api_error, ApiError::ContextWindowExceeded));
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
