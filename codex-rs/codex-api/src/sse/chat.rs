//! Chat Completions SSE stream processing
//!
//! Handles the simpler Chat Completions SSE format:
//! - `data: {json}\n\n` lines (no event prefix)
//! - `data: [DONE]\n\n` sentinel
//! - ChatCompletionChunk parsing
//! - ResponseEvent emission

use crate::chat_response::ChatCompletionChunk;
use crate::common::ResponseEvent;
use crate::error::ApiError;
use codex_client::ByteStream;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::debug;
use tracing::trace;

const RESPONSE_STREAM_CHANNEL_CAPACITY: usize = 1600;

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

    let (tx_event, rx_event) = mpsc::channel::<Result<ResponseEvent, ApiError>>(RESPONSE_STREAM_CHANNEL_CAPACITY);
    tokio::spawn(async move {
        process_chat_completions_sse(
            stream_response.bytes,
            tx_event,
            idle_timeout,
            telemetry,
        ).await;
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
    let mut error: Option<ApiError> = None;

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
                let err = error.unwrap_or(ApiError::Stream(
                    "stream closed before completion".into(),
                ));
                let _ = tx_event.send(Err(err)).await;
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

        // Handle [DONE] sentinel
        if sse.data.trim() == "[DONE]" {
            debug!("Chat Completions stream terminated by [DONE] sentinel");
            return;
        }

        // Parse ChatCompletionChunk
        let chunk = match serde_json::from_str::<ChatCompletionChunk>(&sse.data) {
            Ok(chunk) => chunk,
            Err(e) => {
                debug!("Failed to parse ChatCompletionChunk: {e}, data: {}", &sse.data);
                continue;
            }
        };

        // Convert chunk to ResponseEvent
        if let Some(event) = crate::chat_response::convert_chat_sse_chunk_to_event(chunk) {
            // Check if this is a completion event
            let is_completed = matches!(event, ResponseEvent::Completed { .. });
            let _ = tx_event.send(Ok(event)).await;
            if is_completed {
                debug!("Chat Completions stream completed");
                return;
            }
        }
    }
}
