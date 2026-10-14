use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::SystemTime,
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc};
use tt_domain::errors::DomainError;
use tt_ports::repositories::chat_completion_repository::{
    ChatCompletionApiConfig, ChatCompletionCancelReceiver, ChatCompletionRepository,
    ChatCompletionRepositoryGenerateResponse, ChatCompletionSource, ChatCompletionStreamDelta,
    ChatCompletionStreamSender,
};

const FRONTEND_LIMIT: usize = 800;
const BACKEND_LIMIT: usize = 800;
const LLM_DEFAULT_KEEP: usize = 5;
const LLM_MAX_KEEP: usize = 100;
const BACKEND_EVENT: &str = "tauritavern-backend-log";
const BACKEND_ERROR_EVENT: &str = "tauritavern-backend-error";
const LLM_EVENT: &str = "tauritavern-llm-api-log";
const MAX_PENDING_BACKEND_ERRORS: usize = 50;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrontendLogEntry {
    pub id: u64,
    pub timestamp_ms: i64,
    pub level: String,
    pub message: String,
    pub target: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrontendLogInput {
    pub level: String,
    pub message: String,
    pub target: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackendLogEntry {
    pub id: u64,
    pub timestamp_ms: i64,
    pub level: String,
    pub target: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmLogIndexEntry {
    pub id: u64,
    pub timestamp_ms: i64,
    pub level: String,
    pub ok: bool,
    pub source: String,
    pub model: Option<String>,
    pub endpoint: String,
    pub duration_ms: u32,
    pub stream: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmLogPreview {
    #[serde(flatten)]
    pub index: LlmLogIndexEntry,
    pub error_message: Option<String>,
    pub request_readable: String,
    pub response_readable: String,
    pub response_raw_kind: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmLogRaw {
    pub id: u64,
    pub request_raw: String,
    pub response_raw: String,
    pub response_raw_kind: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LlmLogEntry {
    preview: LlmLogPreview,
    raw: LlmLogRaw,
}

#[derive(Default)]
struct LogState {
    frontend: VecDeque<FrontendLogEntry>,
    backend: VecDeque<BackendLogEntry>,
    llm: VecDeque<LlmLogEntry>,
    pending_backend_errors: VecDeque<String>,
    backend_error_bridge_ready: bool,
    event_senders: HashMap<String, broadcast::Sender<Value>>,
}

pub struct WebDevObservability {
    state: Mutex<LogState>,
    next_id: AtomicU64,
    backend_stream_enabled: AtomicBool,
    llm_stream_enabled: AtomicBool,
    llm_keep: AtomicU64,
    data_root: PathBuf,
    llm_store_path: PathBuf,
}

impl WebDevObservability {
    pub fn new(data_root: PathBuf) -> Arc<Self> {
        let llm_store_path = data_root.join("_tauritavern").join("web-llm-api-logs.json");
        let mut state = LogState::default();
        if let Ok(contents) = std::fs::read_to_string(&llm_store_path)
            && let Ok(entries) = serde_json::from_str::<Vec<LlmLogEntry>>(&contents)
        {
            state.llm = entries.into();
        }
        let next_id = state
            .llm
            .back()
            .map(|entry| entry.preview.index.id.saturating_add(1))
            .unwrap_or(1);
        Arc::new(Self {
            state: Mutex::new(state),
            next_id: AtomicU64::new(next_id),
            backend_stream_enabled: AtomicBool::new(false),
            llm_stream_enabled: AtomicBool::new(false),
            llm_keep: AtomicU64::new(LLM_DEFAULT_KEEP as u64),
            data_root,
            llm_store_path,
        })
    }

    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    fn event_sender(state: &mut LogState, event: &str) -> broadcast::Sender<Value> {
        state
            .event_senders
            .entry(event.to_string())
            .or_insert_with(|| broadcast::channel(64).0)
            .clone()
    }

    pub fn subscribe(&self, event: &str) -> Option<broadcast::Receiver<Value>> {
        if !matches!(event, BACKEND_EVENT | BACKEND_ERROR_EVENT | LLM_EVENT) {
            return None;
        }
        let mut state = self.state.lock().ok()?;
        Some(Self::event_sender(&mut state, event).subscribe())
    }

    pub fn mark_backend_error_bridge_ready_and_drain(&self) -> Vec<String> {
        let Ok(mut state) = self.state.lock() else {
            return Vec::new();
        };
        state.backend_error_bridge_ready = true;
        state.pending_backend_errors.drain(..).collect()
    }

    pub fn emit_or_queue_backend_error(&self, message: &str) {
        let message = message.trim();
        if message.is_empty() {
            return;
        }
        let message = truncate(message, 3072);
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if !state.backend_error_bridge_ready {
            state.pending_backend_errors.push_back(message);
            while state.pending_backend_errors.len() > MAX_PENDING_BACKEND_ERRORS {
                state.pending_backend_errors.pop_front();
            }
            return;
        }

        let _ =
            Self::event_sender(&mut state, BACKEND_ERROR_EVENT).send(json!({ "message": message }));
    }

    pub fn set_backend_stream_enabled(&self, enabled: bool) {
        self.backend_stream_enabled
            .store(enabled, Ordering::Relaxed);
    }

    pub fn set_llm_stream_enabled(&self, enabled: bool) {
        self.llm_stream_enabled.store(enabled, Ordering::Relaxed);
    }

    pub fn set_llm_keep(&self, keep: usize) {
        self.llm_keep
            .store(keep.clamp(1, LLM_MAX_KEEP) as u64, Ordering::Relaxed);
        if let Ok(mut state) = self.state.lock() {
            trim_llm(
                &mut state.llm,
                self.llm_keep.load(Ordering::Relaxed) as usize,
            );
        }
    }

    pub fn append_frontend(&self, entries: Vec<FrontendLogInput>) {
        if let Ok(mut state) = self.state.lock() {
            for entry in entries {
                state.frontend.push_back(FrontendLogEntry {
                    id: self.next_id(),
                    timestamp_ms: now_ms(),
                    level: normalize_level(&entry.level),
                    message: truncate(&entry.message, 3072),
                    target: entry.target.map(|value| truncate(&value, 256)),
                });
                while state.frontend.len() > FRONTEND_LIMIT {
                    state.frontend.pop_front();
                }
            }
        }
    }

    pub fn record_backend(&self, level: &str, target: &str, message: &str) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let entry = BackendLogEntry {
            id: self.next_id(),
            timestamp_ms: now_ms(),
            level: normalize_backend_level(level),
            target: truncate(target, 256),
            message: truncate(message, 3072),
        };
        state.backend.push_back(entry.clone());
        while state.backend.len() > BACKEND_LIMIT {
            state.backend.pop_front();
        }
        if self.backend_stream_enabled.load(Ordering::Relaxed) {
            let _ = Self::event_sender(&mut state, BACKEND_EVENT).send(json!(entry));
        }
    }

    pub fn backend_tail(&self, limit: usize) -> Vec<BackendLogEntry> {
        self.state
            .lock()
            .map(|state| tail(&state.backend, limit))
            .unwrap_or_default()
    }

    pub fn llm_index(&self, limit: usize) -> Vec<LlmLogIndexEntry> {
        self.state
            .lock()
            .map(|state| {
                tail(&state.llm, limit)
                    .into_iter()
                    .map(|entry| entry.preview.index)
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn llm_preview(&self, id: u64) -> Option<LlmLogPreview> {
        self.state
            .lock()
            .ok()?
            .llm
            .iter()
            .find(|entry| entry.preview.index.id == id)
            .map(|entry| entry.preview.clone())
    }

    pub fn llm_raw(&self, id: u64) -> Option<LlmLogRaw> {
        self.state
            .lock()
            .ok()?
            .llm
            .iter()
            .find(|entry| entry.preview.index.id == id)
            .map(|entry| entry.raw.clone())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn record_llm(
        &self,
        source: ChatCompletionSource,
        config: &ChatCompletionApiConfig,
        endpoint_path: &str,
        request: &Value,
        response: Result<&Value, String>,
        duration_ms: u32,
        stream: bool,
    ) {
        let id = self.next_id();
        let timestamp_ms = now_ms();
        let sanitized_request = redact_sensitive_fields(request);
        let request_raw = serde_json::to_string_pretty(&sanitized_request)
            .unwrap_or_else(|_| sanitized_request.to_string());
        let (ok, level, error_message, response_raw) = match response {
            Ok(body) => (
                true,
                "INFO".to_string(),
                None,
                serde_json::to_string_pretty(body).unwrap_or_else(|_| body.to_string()),
            ),
            Err(error) => (false, "ERROR".to_string(), Some(error), String::new()),
        };
        let model = sanitized_request
            .get("model")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let endpoint = sanitized_endpoint(&config.base_url, endpoint_path);
        let index = LlmLogIndexEntry {
            id,
            timestamp_ms,
            level,
            ok,
            source: source.key().to_string(),
            model,
            endpoint,
            duration_ms,
            stream,
        };
        let preview = LlmLogPreview {
            index,
            error_message,
            request_readable: request_raw.clone(),
            response_readable: response_raw.clone(),
            response_raw_kind: Some(if stream { "sse" } else { "json" }.to_string()),
        };
        let entry = LlmLogEntry {
            preview: preview.clone(),
            raw: LlmLogRaw {
                id,
                request_raw,
                response_raw,
                response_raw_kind: Some(if stream { "sse" } else { "json" }.to_string()),
            },
        };
        let persisted = {
            let Ok(mut state) = self.state.lock() else {
                return;
            };
            state.llm.push_back(entry);
            trim_llm(
                &mut state.llm,
                self.llm_keep.load(Ordering::Relaxed) as usize,
            );
            let persisted = state.llm.iter().cloned().collect::<Vec<_>>();
            if self.llm_stream_enabled.load(Ordering::Relaxed) {
                let _ = Self::event_sender(&mut state, LLM_EVENT).send(json!(preview.index));
            }
            persisted
        };
        let Ok(serialized) = serde_json::to_vec(&persisted) else {
            return;
        };
        if let Some(parent) = self.llm_store_path.parent()
            && tokio::fs::create_dir_all(parent).await.is_err()
        {
            return;
        }
        let _ = tokio::fs::write(&self.llm_store_path, serialized).await;
    }

    pub async fn export_bundle(
        &self,
        supplied_frontend: Vec<Value>,
        backend_entries: Vec<BackendLogEntry>,
    ) -> Result<PathBuf, String> {
        let (frontend, backend, llm) = {
            let state = self.state.lock().map_err(|_| "Log store is unavailable")?;
            (
                if supplied_frontend.is_empty() {
                    serde_json::to_value(tail(&state.frontend, FRONTEND_LIMIT))
                        .map_err(|error| error.to_string())?
                } else {
                    Value::Array(supplied_frontend)
                },
                serde_json::to_value(if backend_entries.is_empty() {
                    tail(&state.backend, BACKEND_LIMIT)
                } else {
                    backend_entries
                })
                .map_err(|error| error.to_string())?,
                serde_json::to_value(tail(&state.llm, LLM_MAX_KEEP))
                    .map_err(|error| error.to_string())?,
            )
        };
        let export_dir = self.data_root.join("_tauritavern").join("web-dev-bundles");
        tokio::fs::create_dir_all(&export_dir)
            .await
            .map_err(|error| error.to_string())?;
        let output = export_dir.join(format!("tauritavern-dev-bundle-{}.zip", now_ms()));
        let output_for_worker = output.clone();
        tokio::task::spawn_blocking(move || {
            write_bundle(&output_for_worker, frontend, backend, llm)
        })
        .await
        .map_err(|error| error.to_string())??;
        Ok(output)
    }
}

pub struct LoggingChatCompletionRepository {
    inner: Arc<dyn ChatCompletionRepository>,
    logs: Arc<WebDevObservability>,
}

impl LoggingChatCompletionRepository {
    pub fn new(inner: Arc<dyn ChatCompletionRepository>, logs: Arc<WebDevObservability>) -> Self {
        Self { inner, logs }
    }
}

#[async_trait]
impl ChatCompletionRepository for LoggingChatCompletionRepository {
    async fn list_models(
        &self,
        source: ChatCompletionSource,
        config: &ChatCompletionApiConfig,
    ) -> Result<Value, DomainError> {
        self.inner.list_models(source, config).await
    }

    async fn generate(
        &self,
        source: ChatCompletionSource,
        config: &ChatCompletionApiConfig,
        endpoint_path: &str,
        payload: &Value,
    ) -> Result<ChatCompletionRepositoryGenerateResponse, DomainError> {
        let started = std::time::Instant::now();
        let result = self
            .inner
            .generate(source, config, endpoint_path, payload)
            .await;
        self.logs
            .record_llm(
                source,
                config,
                endpoint_path,
                payload,
                result
                    .as_ref()
                    .map(|value| &value.body)
                    .map_err(ToString::to_string),
                elapsed_ms(started),
                false,
            )
            .await;
        result
    }

    async fn generate_stream(
        &self,
        source: ChatCompletionSource,
        config: &ChatCompletionApiConfig,
        endpoint_path: &str,
        payload: &Value,
        sender: ChatCompletionStreamSender,
        cancel: ChatCompletionCancelReceiver,
    ) -> Result<(), DomainError> {
        let started = std::time::Instant::now();
        let (logged_sender, mut receiver) = mpsc::unbounded_channel::<String>();
        let forwarder = tokio::spawn(async move {
            let mut chunks = Vec::new();
            while let Some(chunk) = receiver.recv().await {
                chunks.push(chunk.clone());
                if sender.send(chunk).is_err() {
                    break;
                }
            }
            chunks
        });
        let result = self
            .inner
            .generate_stream(
                source,
                config,
                endpoint_path,
                payload,
                logged_sender,
                cancel,
            )
            .await;
        let chunks = forwarder.await.unwrap_or_default();
        let response = json!({ "chunks": chunks });
        self.logs
            .record_llm(
                source,
                config,
                endpoint_path,
                payload,
                match &result {
                    Ok(()) => Ok(&response),
                    Err(error) => Err(error.to_string()),
                },
                elapsed_ms(started),
                true,
            )
            .await;
        result
    }

    async fn generate_with_deltas(
        &self,
        source: ChatCompletionSource,
        config: &ChatCompletionApiConfig,
        endpoint_path: &str,
        payload: &Value,
        on_delta: &mut (dyn FnMut(ChatCompletionStreamDelta) + Send),
    ) -> Result<ChatCompletionRepositoryGenerateResponse, DomainError> {
        let started = std::time::Instant::now();
        let result = self
            .inner
            .generate_with_deltas(source, config, endpoint_path, payload, on_delta)
            .await;
        self.logs
            .record_llm(
                source,
                config,
                endpoint_path,
                payload,
                result
                    .as_ref()
                    .map(|value| &value.body)
                    .map_err(ToString::to_string),
                elapsed_ms(started),
                true,
            )
            .await;
        result
    }

    async fn close_provider_session(&self, session_id: &str) {
        self.inner.close_provider_session(session_id).await;
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

fn elapsed_ms(started: std::time::Instant) -> u32 {
    started.elapsed().as_millis().min(u128::from(u32::MAX)) as u32
}

fn sanitized_endpoint(base_url: &str, endpoint_path: &str) -> String {
    let joined = format!("{}{}", base_url.trim_end_matches('/'), endpoint_path);
    let Ok(mut url) = url::Url::parse(&joined) else {
        return truncate(&joined, 2048);
    };
    let _ = url.set_username("");
    let _ = url.set_password(None);
    let sensitive = [
        "api_key",
        "apikey",
        "key",
        "token",
        "access_token",
        "password",
        "secret",
    ];
    let retained = url
        .query_pairs()
        .filter(|(name, _)| !sensitive.contains(&name.to_ascii_lowercase().as_str()))
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    if url.query().is_some() {
        url.set_query(None);
        if !retained.is_empty() {
            url.query_pairs_mut().extend_pairs(retained);
        }
    }
    truncate(url.as_str(), 2048)
}

fn redact_sensitive_fields(value: &Value) -> Value {
    match value {
        Value::Object(fields) => {
            let mut sanitized = serde_json::Map::new();
            for (name, value) in fields {
                let normalized = name.to_ascii_lowercase().replace('-', "_");
                let is_sensitive = [
                    "api_key",
                    "apikey",
                    "authorization",
                    "proxy_password",
                    "password",
                    "secret",
                    "token",
                    "access_token",
                ]
                .iter()
                .any(|needle| normalized == *needle || normalized.ends_with(&format!("_{needle}")));
                sanitized.insert(
                    name.clone(),
                    if is_sensitive {
                        Value::String("[redacted]".to_string())
                    } else {
                        redact_sensitive_fields(value)
                    },
                );
            }
            Value::Object(sanitized)
        }
        Value::Array(values) => Value::Array(values.iter().map(redact_sensitive_fields).collect()),
        _ => value.clone(),
    }
}

fn normalize_level(level: &str) -> String {
    match level.trim().to_ascii_lowercase().as_str() {
        "debug" => "debug",
        "warn" | "warning" => "warn",
        "error" => "error",
        _ => "info",
    }
    .to_string()
}

fn normalize_backend_level(level: &str) -> String {
    match level.trim().to_ascii_uppercase().as_str() {
        "DEBUG" => "DEBUG",
        "WARN" | "WARNING" => "WARN",
        "ERROR" => "ERROR",
        _ => "INFO",
    }
    .to_string()
}

fn truncate(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes.saturating_sub(1);
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!("{}…", &value[..end])
}

fn tail<T: Clone>(entries: &VecDeque<T>, limit: usize) -> Vec<T> {
    let start = entries.len().saturating_sub(limit);
    entries.iter().skip(start).cloned().collect()
}

fn trim_llm(entries: &mut VecDeque<LlmLogEntry>, limit: usize) {
    while entries.len() > limit {
        entries.pop_front();
    }
}

fn write_bundle(path: &Path, frontend: Value, backend: Value, llm: Value) -> Result<(), String> {
    use std::{fs::File, io::Write};
    use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

    let file = File::create(path).map_err(|error| error.to_string())?;
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    let files = [
        ("frontend/logs.json", frontend),
        ("backend/tail.json", backend),
        ("llm-api/logs.json", llm),
    ];
    zip.add_directory("tauritavern-dev-bundle/", options)
        .map_err(|error| error.to_string())?;
    for (name, value) in files {
        zip.start_file(format!("tauritavern-dev-bundle/{name}"), options)
            .map_err(|error| error.to_string())?;
        zip.write_all(
            serde_json::to_string_pretty(&value)
                .map_err(|error| error.to_string())?
                .as_bytes(),
        )
        .map_err(|error| error.to_string())?;
    }
    zip.start_file("tauritavern-dev-bundle/README.txt", options)
        .map_err(|error| error.to_string())?;
    zip.write_all(
        b"TauriTavern browser diagnostic bundle. Logs may contain prompts and model responses.",
    )
    .map_err(|error| error.to_string())?;
    zip.finish().map_err(|error| error.to_string())?;
    Ok(())
}
