use std::{
    collections::HashSet,
    convert::Infallible,
    net::SocketAddr,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, Context as AnyhowContext};
use axum::{
    debug_handler,
    extract::{Path, State},
    http::{header::AUTHORIZATION, HeaderMap, StatusCode},
    response::{
        sse::{Event, Sse},
        IntoResponse, Response,
    },
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::{net::TcpListener, signal, sync::mpsc};
use tokio_stream::{wrappers::ReceiverStream, StreamExt};
use uuid::Uuid;

use crate::{
    chat,
    cli::CliArgs,
    error::Result,
    model,
    session::{HttpSession, SessionConfig},
    vqd,
};

const DEFAULT_LISTEN_ADDR: &str = "127.0.0.1:8080";

#[derive(Clone)]
struct ServerState {
    session_config: SessionConfig,
    default_model: String,
    auth_header: Option<String>,
    allowed_models: Arc<HashSet<&'static str>>,
}

type SharedState = ServerState;

pub async fn run_openai_server(args: &CliArgs) -> Result<()> {
    let listen = args
        .listen
        .clone()
        .unwrap_or_else(|| DEFAULT_LISTEN_ADDR.to_owned());
    let addr: SocketAddr = listen
        .parse()
        .with_context(|| format!("parsing listen address `{listen}`"))?;

    let session_config = args.session_config();
    let default_model = args.model.clone();
    let auth_header = args
        .server_api_key
        .as_ref()
        .map(|key| format!("Bearer {key}"));
    let allowed_models: HashSet<&'static str> = model::MODELS.iter().map(|m| m.id).collect();

    let state = ServerState {
        session_config,
        default_model,
        auth_header,
        allowed_models: Arc::new(allowed_models),
    };

    let router = Router::new()
        .route("/v1/models", get(list_models))
        .route("/v1/models/:model_id", get(get_model))
        .route("/v1/chat/completions", post(chat_completions))
        .with_state(state);

    let listener = TcpListener::bind(addr)
        .await
        .context("binding OpenAI-compatible server address")?;
    println!(
        "OpenAI-compatible service listening on http://{}",
        listener.local_addr().unwrap_or(addr)
    );

    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            if let Err(err) = signal::ctrl_c().await {
                tracing::warn!("failed to listen for shutdown signal: {err:?}");
            }
            println!("Shutdown signal received; stopping server…");
        })
        .await
        .context("running OpenAI-compatible server")?;

    Ok(())
}

type ApiResult<T> = std::result::Result<T, ApiError>;

#[derive(Debug, Serialize)]
struct ApiErrorBody {
    error: ApiErrorDetail,
}

#[derive(Debug, Serialize)]
struct ApiErrorDetail {
    message: String,
    #[serde(rename = "type")]
    error_type: String,
    param: Option<String>,
    code: Option<String>,
}

struct ApiError {
    status: StatusCode,
    body: ApiErrorBody,
}

impl ApiError {
    fn new(status: StatusCode, error_type: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            body: ApiErrorBody {
                error: ApiErrorDetail {
                    message: message.into(),
                    error_type: error_type.to_string(),
                    param: None,
                    code: None,
                },
            },
        }
    }

    fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_request_error", message)
    }

    fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "authentication_error", message)
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found_error", message)
    }

    fn internal(message: impl Into<String>) -> Self {
        let message = message.into();
        tracing::error!("internal server error: {message}");
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", message)
    }

    fn upstream(status: u16, body: String) -> Self {
        let status_code = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
        let truncated = body.chars().take(5000).collect::<String>();
        tracing::warn!(
            "upstream duck.ai error status={} body_len={} snippet={}",
            status,
            body.len(),
            truncated
        );
        Self::new(
            if status_code.is_client_error() {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::BAD_GATEWAY
            },
            "upstream_error",
            format!("Upstream duck.ai error (status {status}): {truncated}"),
        )
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}

async fn list_models(State(state): State<SharedState>, headers: HeaderMap) -> Response {
    if let Err(err) = authorize(&state, &headers) {
        return err.into_response();
    }

    let data: Vec<Value> = model::MODELS
        .iter()
        .map(|m| {
            json!({
                "id": m.id,
                "object": m.object,
                "created": m.created,
                "owned_by": m.owned_by,
            })
        })
        .collect();

    Json(json!({
        "object": "list",
        "data": data,
    }))
    .into_response()
}

async fn get_model(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(model_id): Path<String>,
) -> Response {
    if let Err(err) = authorize(&state, &headers) {
        return err.into_response();
    }

    match model::MODELS.iter().find(|m| m.id == model_id) {
        Some(model) => Json(json!({
            "id": model.id,
            "object": model.object,
            "created": model.created,
            "owned_by": model.owned_by,
        }))
        .into_response(),
        None => ApiError::not_found(format!("Unknown model `{model_id}`")).into_response(),
    }
}

#[derive(Debug, Deserialize)]
struct ChatCompletionRequest {
    model: Option<String>,
    messages: Vec<IncomingMessage>,
    #[serde(default)]
    stream: bool,
}

#[derive(Debug, Deserialize)]
struct IncomingMessage {
    role: String,
    #[serde(default)]
    content: ChatMessageContent,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ChatMessageContent {
    Text(String),
    Parts(Vec<ChatMessagePart>),
}

impl Default for ChatMessageContent {
    fn default() -> Self {
        ChatMessageContent::Text(String::new())
    }
}

#[derive(Debug, Deserialize)]
struct ChatMessagePart {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
}

impl ChatMessageContent {
    fn render(&self) -> String {
        match self {
            ChatMessageContent::Text(text) => text.trim().to_owned(),
            ChatMessageContent::Parts(parts) => {
                let mut segments = Vec::new();
                for part in parts {
                    if part.kind == "text" {
                        if let Some(value) = &part.text {
                            let trimmed = value.trim();
                            if !trimmed.is_empty() {
                                segments.push(trimmed.to_owned());
                            }
                        }
                    }
                }
                segments.join("\n")
            }
        }
    }
}

#[debug_handler]
async fn chat_completions(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Json(request): Json<ChatCompletionRequest>,
) -> Response {
    if let Err(err) = authorize(&state, &headers) {
        return err.into_response();
    }

    if request.stream {
        chat_completions_stream(state, request).await
    } else {
        match chat_completions_non_stream(&state, request).await {
            Ok(response) => Json(response).into_response(),
            Err(err) => err.into_response(),
        }
    }
}

async fn chat_completions_non_stream(
    state: &ServerState,
    request: ChatCompletionRequest,
) -> ApiResult<ChatCompletionResponse> {
    if request.messages.is_empty() {
        return Err(ApiError::bad_request("messages array must not be empty"));
    }

    let model_id = request
        .model
        .clone()
        .unwrap_or_else(|| state.default_model.clone());
    if !state.allowed_models.contains(model_id.as_str()) {
        return Err(ApiError::bad_request(format!(
            "model `{model_id}` is not supported"
        )));
    }

    let prompt = render_conversation(&request.messages)?;

    let session = HttpSession::new(&state.session_config)
        .map_err(|err| ApiError::internal(format!("failed to create HTTP session: {err}")))?;
    let vqd = vqd::prepare_session(&session)
        .await
        .map_err(|err| ApiError::internal(format!("failed to prepare VQD session: {err}")))?;
    let chat_response = chat::send_chat(&session, &vqd, &prompt, &model_id, None)
        .await
        .map_err(|err| ApiError::internal(format!("chat request failed: {err}")))?;

    if chat_response.status != 200 {
        return Err(ApiError::upstream(chat_response.status, chat_response.body));
    }

    let aggregated = extract_completion(&chat_response.body);
    let created = current_unix_time();
    let id = format!("chatcmpl-{}", Uuid::new_v4());

    Ok(ChatCompletionResponse {
        id,
        object: "chat.completion",
        created,
        model: model_id,
        choices: vec![ChatCompletionChoice {
            index: 0,
            message: AssistantMessage {
                role: "assistant",
                content: aggregated,
            },
            finish_reason: Some("stop".to_owned()),
            logprobs: None,
        }],
        usage: Usage {
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
        },
        system_fingerprint: None,
    })
}

async fn chat_completions_stream(state: ServerState, request: ChatCompletionRequest) -> Response {
    if request.messages.is_empty() {
        return ApiError::bad_request("messages array must not be empty").into_response();
    }

    let model_id = request
        .model
        .clone()
        .unwrap_or_else(|| state.default_model.clone());
    if !state.allowed_models.contains(model_id.as_str()) {
        return ApiError::bad_request(format!("model `{model_id}` is not supported"))
            .into_response();
    }

    let prompt = match render_conversation(&request.messages) {
        Ok(value) => value,
        Err(err) => return err.into_response(),
    };

    let (sender, receiver) = mpsc::channel::<String>(128);
    let task_sender = sender.clone();
    tokio::spawn(async move {
        if let Err(err) = stream_chat_worker(state, prompt, model_id, task_sender.clone()).await {
            let error_json = json!({
                "action": "error",
                "message": err.to_string(),
            });
            let _ = task_sender.send(error_json.to_string()).await;
            let _ = task_sender.send("[DONE]".to_owned()).await;
        }
    });
    drop(sender);

    let stream = ReceiverStream::new(receiver)
        .map(|payload| Ok::<Event, Infallible>(Event::default().data(payload)));
    Sse::new(stream).into_response()
}

async fn stream_chat_worker(
    state: ServerState,
    prompt: String,
    model_id: String,
    sender: mpsc::Sender<String>,
) -> crate::error::Result<()> {
    let (raw_tx, mut raw_rx) = mpsc::channel::<String>(128);
    let stream_id = format!("chatcmpl-{}", Uuid::new_v4());
    let start_created = current_unix_time();
    let formatter_sender = sender.clone();
    let formatter = StreamFormatter::new(stream_id, model_id.clone(), start_created);

    tokio::spawn(async move {
        let sender = formatter_sender;
        let mut formatter = formatter;
        while let Some(payload) = raw_rx.recv().await {
            if payload == "[DONE]" {
                if let Some(final_chunk) = formatter.finish_chunk("stop") {
                    let _ = sender.send(final_chunk).await;
                }
                let _ = sender.send("[DONE]".to_owned()).await;
                return;
            }

            match formatter.process_payload(&payload) {
                Ok(chunks) => {
                    for chunk in chunks {
                        if sender.send(chunk).await.is_err() {
                            return;
                        }
                    }
                }
                Err(err) => {
                    tracing::warn!("Failed to process upstream chunk: {err}");
                }
            }
        }

        if let Some(final_chunk) = formatter.finish_chunk("stop") {
            let _ = sender.send(final_chunk).await;
        }
        let _ = sender.send("[DONE]".to_owned()).await;
    });

    let session =
        HttpSession::new(&state.session_config).context("failed to create HTTP session")?;
    let vqd = vqd::prepare_session(&session)
        .await
        .context("failed to prepare VQD session")?;

    let chat_response = chat::send_chat(&session, &vqd, &prompt, &model_id, Some(raw_tx))
        .await
        .context("chat request failed")?;

    if chat_response.status != 200 {
        let truncated = chat_response.body.chars().take(5000).collect::<String>();
        return Err(anyhow!(
            "Upstream duck.ai error (status {}): {}",
            chat_response.status,
            truncated
        ));
    }

    Ok(())
}

fn render_conversation(messages: &[IncomingMessage]) -> ApiResult<String> {
    let mut sections = Vec::new();
    let mut has_user = false;

    for message in messages {
        let text = message.content.render();
        if text.is_empty() {
            continue;
        }
        let label = match message.role.as_str() {
            "system" => "System",
            "assistant" => "Assistant",
            "user" => {
                has_user = true;
                "User"
            }
            other => other,
        };
        sections.push(format!("{label}: {text}"));
    }

    if !has_user {
        return Err(ApiError::bad_request(
            "at least one user message is required",
        ));
    }

    if sections.is_empty() {
        return Err(ApiError::bad_request("no usable message content provided"));
    }

    Ok(sections.join("\n\n"))
}

fn extract_completion(body: &str) -> String {
    let mut assembled = String::new();

    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let data = trimmed
            .strip_prefix("data:")
            .map(str::trim)
            .unwrap_or(trimmed);
        if data == "[DONE]" {
            break;
        }

        if let Ok(json) = serde_json::from_str::<Value>(data) {
            if let Some(text) = json.get("message").and_then(Value::as_str) {
                append_segment(&mut assembled, text);
                continue;
            }
            if let Some(text) = json.get("content").and_then(|v| {
                if v.is_array() {
                    v.as_array().map(|items| {
                        items
                            .iter()
                            .filter_map(|item| item.get("text").and_then(Value::as_str))
                            .collect::<Vec<_>>()
                            .join("")
                    })
                } else {
                    v.as_str().map(|s| s.to_owned())
                }
            }) {
                if !text.is_empty() {
                    append_segment(&mut assembled, text.trim());
                }
                continue;
            }
            if let Some(text) = json.get("body").and_then(Value::as_str) {
                append_segment(&mut assembled, text);
                continue;
            }
        }

        append_segment(&mut assembled, data);
    }

    let trimmed = assembled.trim();
    if trimmed.is_empty() {
        body.trim().to_owned()
    } else {
        trimmed.to_owned()
    }
}

fn append_segment(buffer: &mut String, segment: &str) {
    let segment = segment.trim();
    if segment.is_empty() {
        return;
    }
    if !buffer.is_empty() {
        buffer.push('\n');
    }
    buffer.push_str(segment);
}

#[derive(Clone, Debug, Serialize)]
struct ChatCompletionResponse {
    id: String,
    object: &'static str,
    created: u64,
    model: String,
    choices: Vec<ChatCompletionChoice>,
    usage: Usage,
    #[serde(skip_serializing_if = "Option::is_none")]
    system_fingerprint: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
struct ChatCompletionChoice {
    index: u32,
    message: AssistantMessage,
    #[serde(skip_serializing_if = "Option::is_none")]
    finish_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    logprobs: Option<Value>,
}

#[derive(Clone, Debug, Serialize)]
struct AssistantMessage {
    role: &'static str,
    content: String,
}

#[derive(Clone, Debug, Serialize)]
struct Usage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
}

struct StreamFormatter {
    id: String,
    model: String,
    created: u64,
    sent_role: bool,
    finished: bool,
}

impl StreamFormatter {
    fn new(id: String, model: String, created: u64) -> Self {
        Self {
            id,
            model,
            created,
            sent_role: false,
            finished: false,
        }
    }

    fn process_payload(&mut self, payload: &str) -> crate::error::Result<Vec<String>> {
        let trimmed = payload.trim();
        if trimmed.is_empty() {
            return Ok(Vec::new());
        }

        let value: Value = serde_json::from_str(trimmed)?;
        if let Some(model) = value.get("model").and_then(|v| v.as_str()) {
            if !model.is_empty() {
                self.model = model.to_owned();
            }
        }
        if let Some(created_ms) = value.get("created").and_then(|v| v.as_i64()) {
            if created_ms > 0 {
                self.created = (created_ms / 1000) as u64;
            }
        }

        let action = value.get("action").and_then(|v| v.as_str()).unwrap_or("");
        let role = value
            .get("role")
            .and_then(|v| v.as_str())
            .unwrap_or("assistant");
        let message = value.get("message").and_then(|v| v.as_str()).unwrap_or("");

        let mut chunks = Vec::new();

        if action == "success" {
            if !self.sent_role {
                chunks.push(self.build_role_chunk(role));
                self.sent_role = true;
            }
            if !message.is_empty() {
                chunks.push(self.build_content_chunk(message));
            }
        } else if action == "error" {
            let error_message = if message.is_empty() {
                "upstream error"
            } else {
                message
            };
            chunks.push(self.build_content_chunk(error_message));
            if let Some(final_chunk) = self.finish_chunk("error") {
                chunks.push(final_chunk);
            }
        }

        Ok(chunks)
    }

    fn finish_chunk(&mut self, reason: &str) -> Option<String> {
        if self.finished {
            return None;
        }
        self.finished = true;
        Some(self.build_chunk(json!({}), Some(reason), true))
    }

    fn build_role_chunk(&self, role: &str) -> String {
        self.build_chunk(json!({ "role": role }), None, false)
    }

    fn build_content_chunk(&self, content: &str) -> String {
        self.build_chunk(json!({ "content": content }), None, false)
    }

    fn build_chunk(
        &self,
        delta: Value,
        finish_reason: Option<&str>,
        include_usage: bool,
    ) -> String {
        let mut chunk = json!({
            "id": self.id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.model,
            "choices": [
                {
                    "index": 0,
                    "delta": delta,
                    "finish_reason": finish_reason.map(Value::from).unwrap_or(Value::Null),
                    "logprobs": Value::Null
                }
            ],
        });

        if include_usage {
            chunk["usage"] = json!({
                "prompt_tokens": 0,
                "completion_tokens": 0,
                "total_tokens": 0,
            });
        }

        chunk.to_string()
    }
}

fn authorize(state: &ServerState, headers: &HeaderMap) -> ApiResult<()> {
    if let Some(expected) = &state.auth_header {
        let provided = headers
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .map(str::trim);
        match provided {
            Some(value) if value == expected => Ok(()),
            Some(_) => Err(ApiError::unauthorized("invalid API key provided")),
            None => Err(ApiError::unauthorized(
                "missing Authorization header with Bearer token",
            )),
        }
    } else {
        Ok(())
    }
}

fn current_unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}
