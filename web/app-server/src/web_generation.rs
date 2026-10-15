use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, RwLock};
use tt_application::dto::chat_completion_dto::ChatCompletionGenerateRequestDto;
use tt_application::services::chat_completion_service::ChatCompletionService;
use uuid::Uuid;

const MAX_RESULT_BYTES: u64 = 32 * 1024 * 1024;
const UNCLAIMED_TTL: u64 = 7 * 24 * 60 * 60;
const CONFIRMED_TTL: u64 = 24 * 60 * 60;

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct JobMeta {
    task_id: String,
    chat_key: String,
    revision: String,
    request_digest: String,
    #[serde(default)]
    source: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    custom_api_format: String,
    #[serde(default)]
    opencode_api_format: String,
    status: String,
    last_seq: u64,
    created_at: u64,
    updated_at: u64,
    confirmed_at: Option<u64>,
    #[serde(default)]
    claim_token: Option<String>,
    #[serde(default)]
    claim_expires_at: Option<u64>,
}

struct Job {
    meta: Mutex<JobMeta>,
    file: PathBuf,
}

pub(crate) struct WebGenerationRegistry {
    root: PathBuf,
    jobs: RwLock<HashMap<String, Arc<Job>>>,
    service: Arc<ChatCompletionService>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn valid_id(task_id: &str) -> Result<(), String> {
    if Uuid::parse_str(task_id).is_err() || task_id.len() != 36 {
        return Err("Invalid web generation task id".to_string());
    }
    Ok(())
}

fn digest(dto: &ChatCompletionGenerateRequestDto) -> Result<String, String> {
    let bytes = serde_json::to_vec(dto).map_err(|error| error.to_string())?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn public_meta(meta: &JobMeta) -> Value {
    json!({
        "taskId": meta.task_id,
        "chatKey": meta.chat_key,
        "revision": meta.revision,
        "source": meta.source,
        "model": meta.model,
        "customApiFormat": meta.custom_api_format,
        "opencodeApiFormat": meta.opencode_api_format,
        "status": meta.status,
        "lastSeq": meta.last_seq,
        "createdAt": meta.created_at,
        "updatedAt": meta.updated_at,
        "confirmedAt": meta.confirmed_at,
    })
}

impl WebGenerationRegistry {
    pub(crate) async fn load(
        root: PathBuf,
        service: Arc<ChatCompletionService>,
    ) -> Result<Arc<Self>, String> {
        tokio::fs::create_dir_all(&root)
            .await
            .map_err(|error| error.to_string())?;
        let registry = Arc::new(Self {
            root,
            jobs: RwLock::new(HashMap::new()),
            service,
        });
        let mut directory = tokio::fs::read_dir(&registry.root)
            .await
            .map_err(|error| error.to_string())?;
        while let Some(entry) = directory
            .next_entry()
            .await
            .map_err(|error| error.to_string())?
        {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let raw = tokio::fs::read(&path)
                .await
                .map_err(|error| error.to_string())?;
            let mut meta: JobMeta = serde_json::from_slice(&raw)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            valid_id(&meta.task_id)?;
            let expired = now().saturating_sub(meta.confirmed_at.unwrap_or(meta.created_at))
                > if meta.confirmed_at.is_some() {
                    CONFIRMED_TTL
                } else {
                    UNCLAIMED_TTL
                };
            let events_path = registry.events_path(&meta.task_id);
            if expired {
                let _ = tokio::fs::remove_file(&path).await;
                let _ = tokio::fs::remove_file(events_path).await;
                continue;
            }
            if let Ok(raw_events) = tokio::fs::read(&events_path).await {
                let complete_len = raw_events
                    .iter()
                    .rposition(|byte| *byte == b'\n')
                    .map(|index| index + 1)
                    .unwrap_or(0);
                let last_saved_seq = raw_events[..complete_len]
                    .split(|byte| *byte == b'\n')
                    .filter(|line| !line.is_empty())
                    .map(serde_json::from_slice::<Value>)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|error| format!("{}: {error}", events_path.display()))?
                    .last()
                    .and_then(|event| event.get("seq"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                meta.last_seq = last_saved_seq;
            }
            if meta.status == "running" {
                meta.status = "interrupted".to_string();
                meta.updated_at = now();
            }
            registry.save_meta(&meta).await?;
            registry.jobs.write().await.insert(
                meta.task_id.clone(),
                Arc::new(Job {
                    meta: Mutex::new(meta),
                    file: events_path,
                }),
            );
        }
        let weak = Arc::downgrade(&registry);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(60 * 60));
            loop {
                interval.tick().await;
                let Some(registry) = weak.upgrade() else {
                    break;
                };
                registry.cleanup_expired().await;
            }
        });
        Ok(registry)
    }

    async fn cleanup_expired(&self) {
        let mut expired = Vec::new();
        {
            let jobs = self.jobs.read().await;
            for (task_id, job) in jobs.iter() {
                let meta = job.meta.lock().await;
                if meta.status == "running" {
                    continue;
                }
                let age = now().saturating_sub(meta.confirmed_at.unwrap_or(meta.created_at));
                if age
                    > if meta.confirmed_at.is_some() {
                        CONFIRMED_TTL
                    } else {
                        UNCLAIMED_TTL
                    }
                {
                    expired.push(task_id.clone());
                }
            }
        }
        if expired.is_empty() {
            return;
        }
        let mut jobs = self.jobs.write().await;
        for task_id in expired {
            jobs.remove(&task_id);
            let _ = tokio::fs::remove_file(self.meta_path(&task_id)).await;
            let _ = tokio::fs::remove_file(self.events_path(&task_id)).await;
        }
    }

    fn meta_path(&self, task_id: &str) -> PathBuf {
        self.root.join(format!("{task_id}.json"))
    }
    fn events_path(&self, task_id: &str) -> PathBuf {
        self.root.join(format!("{task_id}.jsonl"))
    }

    async fn save_meta(&self, meta: &JobMeta) -> Result<(), String> {
        let bytes = serde_json::to_vec(meta).map_err(|error| error.to_string())?;
        let temporary = self.root.join(format!("{}.tmp", meta.task_id));
        tokio::fs::write(&temporary, bytes)
            .await
            .map_err(|error| error.to_string())?;
        tokio::fs::rename(temporary, self.meta_path(&meta.task_id))
            .await
            .map_err(|error| error.to_string())
    }

    pub(crate) async fn start(
        self: &Arc<Self>,
        task_id: String,
        chat_key: String,
        revision: String,
        dto: ChatCompletionGenerateRequestDto,
    ) -> Result<Value, String> {
        valid_id(&task_id)?;
        if chat_key.trim().is_empty() || chat_key.len() > 512 || revision.len() > 256 {
            return Err("Invalid chat identity or revision".to_string());
        }
        let request_digest = digest(&dto)?;
        let mut jobs = self.jobs.write().await;
        if let Some(job) = jobs.get(&task_id) {
            let meta = job.meta.lock().await;
            if meta.request_digest != request_digest
                || meta.chat_key != chat_key
                || meta.revision != revision
            {
                return Err(
                    "Web generation task id already belongs to a different request".to_string(),
                );
            }
            return Ok(public_meta(&meta));
        }
        for job in jobs.values() {
            let meta = job.meta.lock().await;
            if meta.chat_key == chat_key && meta.status == "running" {
                return Err(format!(
                    "Chat already has a running generation: {}",
                    meta.task_id
                ));
            }
        }
        let meta = JobMeta {
            task_id: task_id.clone(),
            chat_key,
            revision,
            request_digest,
            source: dto
                .get_string("chat_completion_source")
                .unwrap_or_default()
                .to_string(),
            model: [
                "model",
                "openai_model",
                "custom_model",
                "claude_model",
                "google_model",
                "deepseek_model",
            ]
            .iter()
            .find_map(|key| dto.get_string(key).filter(|value| !value.is_empty()))
            .unwrap_or_default()
            .to_string(),
            custom_api_format: dto
                .get_string("custom_api_format")
                .unwrap_or_default()
                .to_string(),
            opencode_api_format: dto
                .get_string("opencode_api_format")
                .unwrap_or_default()
                .to_string(),
            status: "running".to_string(),
            last_seq: 0,
            created_at: now(),
            updated_at: now(),
            confirmed_at: None,
            claim_token: None,
            claim_expires_at: None,
        };
        self.save_meta(&meta).await?;
        let job = Arc::new(Job {
            meta: Mutex::new(meta.clone()),
            file: self.events_path(&task_id),
        });
        jobs.insert(task_id.clone(), job.clone());
        if let Err(error) = self.service.start_stream(task_id.clone(), dto).await {
            let mut current = job.meta.lock().await;
            current.status = "error".to_string();
            current.updated_at = now();
            self.save_meta(&current).await?;
            return Err(error.to_string());
        }
        let registry = self.clone();
        tokio::spawn(async move {
            registry.drain(task_id, job).await;
        });
        Ok(public_meta(&meta))
    }

    async fn drain(self: Arc<Self>, task_id: String, job: Arc<Job>) {
        let mut after_seq = 0;
        loop {
            let result = self.service.read_stream(&task_id, after_seq).await;
            let result = match result {
                Ok(result) => result,
                Err(error) => {
                    self.finish(&job, "error", Some(error.to_string())).await;
                    break;
                }
            };
            let mut batch = Vec::new();
            for event in &result.events {
                let value = match serde_json::to_value(event) {
                    Ok(value) => value,
                    Err(error) => {
                        self.finish(&job, "error", Some(error.to_string())).await;
                        return;
                    }
                };
                if let Some(seq) = value.get("seq").and_then(Value::as_u64) {
                    after_seq = seq;
                }
                batch.push(value);
            }
            if !batch.is_empty()
                && let Err(error) = self.append(&job, &batch).await
            {
                let _ = self.service.close_stream(&task_id).await;
                self.finish(&job, "error", Some(error)).await;
                break;
            }
            let status = serde_json::to_value(result.status).unwrap_or(Value::Null);
            if status != "running" {
                self.finish(&job, status.as_str().unwrap_or("error"), None)
                    .await;
                break;
            }
        }
        let _ = self.service.close_stream(&task_id).await;
    }

    async fn append(&self, job: &Job, batch: &[Value]) -> Result<(), String> {
        let mut meta = job.meta.lock().await;
        let mut bytes = Vec::new();
        for event in batch {
            serde_json::to_writer(&mut bytes, event).map_err(|error| error.to_string())?;
            bytes.push(b'\n');
        }
        let size = tokio::fs::metadata(&job.file)
            .await
            .map(|value| value.len())
            .unwrap_or(0);
        if size + bytes.len() as u64 > MAX_RESULT_BYTES {
            return Err("Web generation result exceeded 32 MiB".to_string());
        }
        let mut file = tokio::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&job.file)
            .await
            .map_err(|error| error.to_string())?;
        file.write_all(&bytes)
            .await
            .map_err(|error| error.to_string())?;
        file.sync_data().await.map_err(|error| error.to_string())?;
        meta.last_seq = batch
            .last()
            .and_then(|value| value.get("seq"))
            .and_then(Value::as_u64)
            .unwrap_or(meta.last_seq);
        meta.updated_at = now();
        self.save_meta(&meta).await
    }

    async fn finish(&self, job: &Job, status: &str, error: Option<String>) {
        if job.meta.lock().await.status != "running" {
            return;
        }
        if let Some(message) = error {
            let seq = job.meta.lock().await.last_seq + 1;
            let _ = self
                .append(job, &[json!({"type":"error","seq":seq,"message":message})])
                .await;
        }
        let mut meta = job.meta.lock().await;
        meta.status = status.to_string();
        meta.updated_at = now();
        let _ = self.save_meta(&meta).await;
    }

    async fn find(&self, task_id: &str) -> Result<Arc<Job>, String> {
        valid_id(task_id)?;
        self.jobs
            .read()
            .await
            .get(task_id)
            .cloned()
            .ok_or_else(|| "Web generation task not found".to_string())
    }

    pub(crate) async fn read(&self, task_id: &str, after_seq: u64) -> Result<Value, String> {
        let job = self.find(task_id).await?;
        let meta_guard = job.meta.lock().await;
        let meta = meta_guard.clone();
        if after_seq > meta.last_seq {
            return Err("Web generation cursor exceeds saved result".to_string());
        }
        let file = match tokio::fs::File::open(&job.file).await {
            Ok(file) => Some(file),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && meta.last_seq == 0 => {
                None
            }
            Err(error) => return Err(error.to_string()),
        };
        let mut events = Vec::new();
        if let Some(file) = file {
            let mut reader = BufReader::new(file);
            let mut line = Vec::new();
            loop {
                line.clear();
                let read = reader
                    .read_until(b'\n', &mut line)
                    .await
                    .map_err(|error| error.to_string())?;
                if read == 0 || !line.ends_with(b"\n") {
                    break;
                }
                let event: Value =
                    serde_json::from_slice(&line).map_err(|error| error.to_string())?;
                if event.get("seq").and_then(Value::as_u64).unwrap_or(0) > after_seq {
                    events.push(event);
                    if events.len() == 64 {
                        break;
                    }
                }
            }
        }
        drop(meta_guard);
        Ok(json!({"events":events,"status":meta.status,"task":public_meta(&meta)}))
    }

    pub(crate) async fn list(&self, chat_key: &str) -> Value {
        let jobs = self.jobs.read().await;
        let mut found = Vec::new();
        for job in jobs.values() {
            let meta = job.meta.lock().await;
            if meta.chat_key == chat_key {
                found.push(meta.clone());
            }
        }
        found.sort_by_key(|meta| meta.created_at);
        json!(found.iter().map(public_meta).collect::<Vec<_>>())
    }

    pub(crate) async fn cancel(&self, task_id: &str) -> Result<Value, String> {
        let job = self.find(task_id).await?;
        let mut meta = job.meta.lock().await;
        let running = meta.status == "running";
        if running {
            meta.status = "cancelled".to_string();
            meta.updated_at = now();
            self.save_meta(&meta).await?;
            drop(meta);
            let _ = self.service.close_stream(task_id).await;
        } else {
            drop(meta);
        }
        Ok(public_meta(&*job.meta.lock().await))
    }

    pub(crate) async fn claim(&self, task_id: &str, token: &str) -> Result<Value, String> {
        valid_id(token)?;
        let job = self.find(task_id).await?;
        let mut meta = job.meta.lock().await;
        if meta.status == "running" || meta.confirmed_at.is_some() {
            return Ok(json!({"acquired":false,"task":public_meta(&meta)}));
        }
        let available = meta.claim_expires_at.unwrap_or(0) <= now()
            || meta.claim_token.as_deref() == Some(token);
        if available {
            meta.claim_token = Some(token.to_string());
            meta.claim_expires_at = Some(now() + 300);
            self.save_meta(&meta).await?;
        }
        Ok(json!({"acquired":available,"task":public_meta(&meta)}))
    }

    pub(crate) async fn confirm_with_token(
        &self,
        task_id: &str,
        token: &str,
    ) -> Result<Value, String> {
        let job = self.find(task_id).await?;
        let mut meta = job.meta.lock().await;
        if meta.status == "running" {
            return Err("Cannot confirm a running generation".to_string());
        }
        if meta.claim_token.as_deref() != Some(token) || meta.claim_expires_at.unwrap_or(0) <= now()
        {
            return Err("Web generation commit claim is missing or expired".to_string());
        }
        if meta.confirmed_at.is_none() {
            meta.confirmed_at = Some(now());
            self.save_meta(&meta).await?;
        }
        Ok(public_meta(&meta))
    }
}
