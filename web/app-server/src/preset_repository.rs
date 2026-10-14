use async_trait::async_trait;
use serde_json::Value;
use std::path::{Path, PathBuf};
use tokio::fs;
use tt_adapter_storage_core::preset_file_naming::{PresetFilePaths, load_named_preset_files};
use tt_domain::{
    errors::DomainError,
    models::preset::{DefaultPreset, Preset, PresetType},
};
use tt_ports::repositories::preset_repository::PresetRepository;

pub struct WebPresetRepository {
    user_dir: PathBuf,
    content_dir: PathBuf,
}

impl WebPresetRepository {
    pub fn new(user_dir: PathBuf, content_dir: PathBuf) -> Self {
        Self {
            user_dir,
            content_dir,
        }
    }

    fn user_directory(&self, preset_type: &PresetType) -> PathBuf {
        self.user_dir.join(preset_type.directory_name())
    }

    fn default_directory(&self, preset_type: &PresetType) -> Option<&'static str> {
        match preset_type {
            PresetType::Kobold => Some("kobold"),
            PresetType::Novel => Some("novel"),
            PresetType::OpenAI => Some("openai"),
            PresetType::TextGen => Some("textgen"),
            PresetType::Instruct => Some("instruct"),
            PresetType::Context => Some("context"),
            PresetType::SysPrompt => Some("sysprompt"),
            PresetType::Reasoning => Some("reasoning"),
        }
    }

    fn preset_paths(
        &self,
        name: &str,
        preset_type: &PresetType,
    ) -> Result<PresetFilePaths, DomainError> {
        PresetFilePaths::new(name, &self.user_directory(preset_type), ".json")
    }
}

#[async_trait]
impl PresetRepository for WebPresetRepository {
    async fn save_preset(&self, preset: &Preset) -> Result<(), DomainError> {
        let directory = self.user_directory(&preset.preset_type);
        fs::create_dir_all(&directory).await.map_err(|error| {
            DomainError::InternalError(format!("Failed to create preset directory: {error}"))
        })?;
        let path = self
            .preset_paths(&preset.name, &preset.preset_type)?
            .prepare_for_save()
            .await?;
        let data = serde_json::to_vec_pretty(&preset.data_with_name())
            .map_err(|error| DomainError::InvalidData(error.to_string()))?;
        fs::write(path, data)
            .await
            .map_err(|error| DomainError::InternalError(format!("Failed to save preset: {error}")))
    }

    async fn delete_preset(&self, name: &str, preset_type: &PresetType) -> Result<(), DomainError> {
        let path = self
            .preset_paths(name, preset_type)?
            .resolve_existing()?
            .ok_or_else(|| DomainError::NotFound(format!("Preset not found: {name}")))?;
        fs::remove_file(path).await.map_err(|error| {
            DomainError::InternalError(format!("Failed to delete preset: {error}"))
        })
    }

    async fn preset_exists(
        &self,
        name: &str,
        preset_type: &PresetType,
    ) -> Result<bool, DomainError> {
        Ok(self
            .preset_paths(name, preset_type)?
            .resolve_existing()?
            .is_some())
    }

    async fn get_preset(
        &self,
        name: &str,
        preset_type: &PresetType,
    ) -> Result<Option<Preset>, DomainError> {
        let Some(path) = self.preset_paths(name, preset_type)?.resolve_existing()? else {
            return Ok(None);
        };
        let content = fs::read_to_string(path).await.map_err(|error| {
            DomainError::InternalError(format!("Failed to read preset: {error}"))
        })?;
        let data = serde_json::from_str::<Value>(&content)
            .map_err(|error| DomainError::InvalidData(format!("Invalid preset JSON: {error}")))?;
        Ok(Some(Preset::new(
            name.to_string(),
            preset_type.clone(),
            data,
        )))
    }

    async fn list_presets(&self, preset_type: &PresetType) -> Result<Vec<String>, DomainError> {
        load_named_preset_files(&self.user_directory(preset_type))
            .await
            .map(|entries| entries.into_iter().map(|entry| entry.name).collect())
    }

    async fn get_default_preset(
        &self,
        name: &str,
        preset_type: &PresetType,
    ) -> Result<Option<DefaultPreset>, DomainError> {
        if name.trim().is_empty() || name.contains('/') || name.contains('\\') {
            return Ok(None);
        }
        let Some(directory) = self.default_directory(preset_type) else {
            return Ok(None);
        };
        let path = self
            .content_dir
            .join("presets")
            .join(directory)
            .join(format!("{name}.json"));
        if !path.starts_with(&self.content_dir) || !path.is_file() {
            return Ok(None);
        }
        let content = fs::read_to_string(&path).await.map_err(|error| {
            DomainError::InternalError(format!("Failed to read default preset: {error}"))
        })?;
        let data = serde_json::from_str::<Value>(&content).map_err(|error| {
            DomainError::InvalidData(format!("Invalid default preset JSON: {error}"))
        })?;
        let preset_name = data
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(name)
            .to_string();
        let filename = Path::new("presets")
            .join(directory)
            .join(format!("{name}.json"))
            .to_string_lossy()
            .replace('\\', "/");
        Ok(Some(DefaultPreset {
            filename,
            name: preset_name,
            preset_type: preset_type.clone(),
            is_default: true,
            data,
        }))
    }
}
