use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context};
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use futures_util::TryStreamExt;
use serde_json::json;
use tokio::sync::mpsc;

use crate::error::Result;
use crate::session::HttpSession;
use crate::vqd::VqdSession;

/// Chat streaming response payload.
#[derive(Debug)]
pub struct ChatResponse {
    pub status: u16,
    pub body: String,
}

/// Send chat prompt using prepared session metadata.
pub async fn send_chat(
    session: &HttpSession,
    vqd: &VqdSession,
    prompt: &str,
    model_id: &str,
    mut event_tx: Option<mpsc::Sender<String>>,
) -> Result<ChatResponse> {
    const MAX_RETRIES: usize = 2;

    let url = session
        .base_url()
        .join("duckchat/v1/chat")
        .context("invalid chat url")?;

    for attempt in 0..=MAX_RETRIES {
        let request = session
            .client()
            .post(url.clone())
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .header("x-fe-version", &vqd.fe_version)
            .header("x-vqd-hash-1", &vqd.vqd_header)
            .header("x-fe-signals", format_fraud_signals());

        let response = request
            .json(&build_chat_payload(prompt, model_id))
            .send()
            .await
            .context("sending chat request")?;

        let status = response.status().as_u16();
        let mut body = String::new();
        let mut sse_buffer = String::new();

        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.try_next().await.context("reading chat stream")? {
            let chunk_str = String::from_utf8_lossy(&chunk);
            body.push_str(&chunk_str);

            if status == 200 {
                if let Some(sender) = event_tx.as_ref() {
                    if !forward_sse_payloads(sender, &mut sse_buffer, &chunk_str).await {
                        // Client dropped; stop forwarding but continue to consume response
                        sse_buffer.clear();
                        event_tx = None;
                    }
                }
            }
        }

        if status == 200 {
            if let Some(sender) = event_tx.as_ref() {
                if !sse_buffer.is_empty() {
                    let _ = emit_event_block(sender, &sse_buffer).await;
                }
                let _ = sender.send("[DONE]".to_owned()).await;
            }
        }

        if status == 418 {
            match serde_json::from_str::<serde_json::Value>(&body) {
                Ok(value) => {
                    tracing::warn!("Received challenge response: {value}");
                    let solved = crate::challenge::handle_challenge(session, &value).await?;
                    if solved {
                        tracing::info!("Challenge solved; retrying chat (attempt {attempt})");
                        continue;
                    }
                }
                Err(err) => {
                    tracing::error!("Failed to parse challenge JSON: {err:?}");
                }
            }
        }

        return Ok(ChatResponse { status, body });
    }

    Err(anyhow!(
        "Reached maximum chat retries after handling challenge"
    ))
}

async fn forward_sse_payloads(
    sender: &mpsc::Sender<String>,
    buffer: &mut String,
    chunk: &str,
) -> bool {
    buffer.push_str(chunk);

    loop {
        let (event_block, consumed) = match extract_event_block(buffer) {
            Some(value) => value,
            None => break,
        };

        if !emit_event_block(sender, &event_block).await {
            return false;
        }

        if consumed >= buffer.len() {
            buffer.clear();
        } else {
            let remaining = buffer[consumed..].to_owned();
            buffer.clear();
            buffer.push_str(&remaining);
        }
    }

    true
}

fn extract_event_block(buffer: &str) -> Option<(String, usize)> {
    if let Some(pos) = buffer.find("\r\n\r\n") {
        let block = buffer[..pos].to_owned();
        return Some((block, pos + 4));
    }
    if let Some(pos) = buffer.find("\n\n") {
        let block = buffer[..pos].to_owned();
        return Some((block, pos + 2));
    }
    None
}

async fn emit_event_block(sender: &mpsc::Sender<String>, block: &str) -> bool {
    for line in block.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(data) = line.strip_prefix("data:") {
            let payload = data.trim_start();
            if sender.send(payload.to_owned()).await.is_err() {
                return false;
            }
        }
    }
    true
}

fn build_chat_payload(prompt: &str, model_id: &str) -> serde_json::Value {
    json!({
        "model": model_id,
        "metadata": serde_json::Map::<String, serde_json::Value>::new(),
        "messages": [
            {
                "role": "user",
                "content": [
                    {
                        "type": "text",
                        "text": prompt,
                    }
                ]
            }
        ],
        "canUseTools": false,
        "canUseApproxLocation": false,
    })
}

fn format_fraud_signals() -> String {
    let start = unix_millis();
    let events = json!([
        { "name": "onboarding_impression", "delta": 180 },
        { "name": "onboarding_finish", "delta": 22_600 },
        { "name": "startNewChat_free", "delta": 22_640 },
    ]);
    let payload = json!({
        "start": start,
        "end": unix_millis(),
        "events": events,
    });
    BASE64_STANDARD.encode(payload.to_string())
}

fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn builds_chat_payload_structure() {
        let payload = build_chat_payload("hi", "gpt-4o-mini");
        assert_eq!(payload["model"], Value::String("gpt-4o-mini".into()));
        assert_eq!(
            payload["messages"][0]["content"][0]["text"],
            Value::String("hi".into())
        );
    }

    #[test]
    fn fraud_signals_is_base64() {
        let signals = format_fraud_signals();
        assert!(BASE64_STANDARD.decode(signals).expect("valid base64").len() > 0);
    }
}
