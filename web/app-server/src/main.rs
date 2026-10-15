mod dev_observability;
mod preset_repository;
mod web_generation;

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    env,
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use axum::{
    Router,
    body::Body,
    extract::{
        DefaultBodyLimit, Json, Query, State,
        ws::{WebSocket, WebSocketUpgrade},
    },
    http::{Request, Response, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use dev_observability::{FrontendLogInput, LoggingChatCompletionRepository, WebDevObservability};
use futures_util::TryStreamExt;
use http::Method;
use preset_repository::WebPresetRepository;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{Mutex, Semaphore, mpsc};
use tt_adapter_archive::FileDataArchiveExecutor;
use tt_adapter_extension::FileExtensionRepository;
use tt_adapter_http::{HttpClientPool, HttpClientProfile};
use tt_adapter_mcp::RmcpMcpGateway;
use tt_adapter_media::{
    FileAvatarRepository, FileBackgroundRepository, FileImageMetadataRepository,
    FilesystemHostResourceStore, FilesystemUserMediaStore,
};
use tt_adapter_provider_http::{
    HttpChatCompletionRepository, HttpEmbeddingRepository, HttpProviderMetadataRepository,
    HttpSearxngSearchRepository, HttpTtsRepository,
};
use tt_adapter_quickjs::QuickJsScriptEngine;
use tt_adapter_storage_core::{
    DataDirectory, FileAssetRepository, FileChatRepository, FileExtensionStoreRepository,
    FileGroupRepository, FileLlmConnectionRepository, FilePromptCacheRepository,
    FileQuickReplyRepository, FileSecretRepository, FileSettingsRepository, FileThemeRepository,
    FileUserEndpointGrantRepository,
    chat_directory_identity::new_shared_chat_alias_store_for_user_dir,
};
use tt_adapter_storage_userdata::{
    FileAgentProfileRepository, FileAgentRepository, FileCharacterRepository, FileSkillRepository,
    FileSpriteRepository, FileWorldInfoRepository,
};
use tt_adapter_tokenization::MiktikTokenizerRepository;
use tt_adapter_triviumdb::TriviumDatabaseBackend;
use tt_adapter_vector::{CandleLocalEmbeddingRepository, RedbVectorRepository};
use tt_application::dto::chat_history_dto::{ChatHistoryLocator, CurrentCommitReason};
use tt_application::services::agent_workspace_lifecycle_service::AgentChatWorkspaceTarget;
use tt_application::{
    dto::{
        agent_dto::*,
        background_dto::{DeleteBackgroundDto, RenameBackgroundDto},
        bootstrap_dto::BootstrapSnapshotDto,
        character_dto::{
            BulkMergeCharacterCardDataDto, CheckCharacterLorebookConflictDto, CreateCharacterDto,
            CreateWithAvatarDto, DeleteCharacterDto, DuplicateCharacterDto,
            ExportCharacterContentDto, ImportCharacterDto, MergeCharacterCardDataDto,
            RenameCharacterDto, ReplaceCharacterDto, ResolveCharacterLorebookConflictDto,
            UpdateAvatarDto, UpdateCharacterCardDataDto, UpdateCharacterDto,
        },
        chat_completion_dto::{ChatCompletionGenerateRequestDto, ChatCompletionStatusRequestDto},
        chat_dto::{
            DeleteGroupChatDto, ImportCharacterChatsDto, ImportGroupChatDto,
            PinnedCharacterChatDto, PinnedGroupChatDto, RenameGroupChatDto,
            RestoreCharacterChatBackupDto, RestoreGroupChatBackupDto,
        },
        group_dto::{CreateGroupDto, DeleteGroupDto, GroupDto, UpdateGroupDto},
        image_metadata_dto::{
            CreateImageMetadataFolderDto, DeleteImageMetadataFolderDto,
            ImageMetadataFolderAssignmentDto, SetImageMetadataFolderThumbnailsDto,
            UpdateImageMetadataFolderDto,
        },
        llm_connection_dto::{
            ListLlmConnectionsResultDto, LlmConnectionIdDto, LoadLlmConnectionResultDto,
            SaveLlmConnectionDto,
        },
        mcp_dto::{
            CallLegacyMcpToolDto, CreateMcpServerDto, McpExecutionCallIdDto, McpRegistrationIdDto,
            McpTestCallIdDto, SetMcpServerStateDto, SetMcpToolDescriptionOverrideDto,
            SetMcpToolPermissionDto, TestMcpToolCallDto, UpdateMcpServerDto,
        },
        preset_dto::{
            DeleteOpenAIPresetDto, DeleteOpenAIPresetResponseDto, DeletePresetDto,
            RestorePresetDto, RestorePresetResponseDto, SaveOpenAIPresetDto, SavePresetDto,
            SavePresetResponseDto,
        },
        provider_metadata_dto::{
            ProviderModelProvidersRequestDto, SiliconFlowEmbeddingModelsRequestDto,
            WorkersAiModelsRequestDto,
        },
        searxng_search_dto::SearxngSearchRequestDto,
        secret_dto::{
            DeleteSecretDto, FindSecretDto, RenameSecretDto, RotateSecretDto, WriteSecretDto,
        },
        settings_dto::{
            SettingsSnapshotDto, UpdateTauriTavernSettingsDto, UserSettingsDto,
            UserSettingsPatchDto,
        },
        sprite_dto::{DeleteSpriteDto, ListSpritesDto, UploadSpriteDto, UploadSpritePackDto},
        tokenization_dto::{
            DecodeTokensRequestDto, EncodeTokensRequestDto, OpenAiLogitBiasRequestDto,
            OpenAiTokenCountBatchRequestDto, OpenAiTokenPrefixCountRequestDto,
        },
        vector_dto::VectorRouteRequestDto,
        world_info_dto::{
            DeleteWorldInfoDto, GetWorldInfoDto, GetWorldInfosBatchDto,
            GetWorldInfosBatchResponseDto, ImportWorldInfoDto, ImportWorldInfoResponseDto,
            NormalizeWorldInfoNameDto, SaveWorldInfoDto,
        },
    },
    errors::ApplicationError,
    services::{
        agent_model_gateway::ChatCompletionAgentModelGateway,
        agent_profile_diagnostic_service::AgentProfileDiagnosticService,
        agent_profile_service::AgentProfileService,
        agent_run_history_service::AgentRunHistoryService,
        agent_runtime_service::AgentRuntimeService,
        agent_workspace_lifecycle_service::{AgentRunActivity, AgentWorkspaceLifecycleService},
        asset_service::AssetService,
        avatar_service::AvatarService,
        background_service::BackgroundService,
        character_service::CharacterService,
        chat_completion_service::ChatCompletionService,
        chat_history_coordinator::ChatHistoryCoordinator,
        chat_payload_commit_service::ChatPayloadCommitService,
        chat_service::ChatService,
        data_archive_service::{DataArchiveJobRegistry, DataArchiveService},
        database_service::DatabaseService,
        extension_service::ExtensionService,
        extension_store_service::ExtensionStoreService,
        group_chat_service::GroupChatService,
        group_service::GroupService,
        host_resource_service::{HostResourceDeliveryCapabilities, HostResourceService},
        image_metadata_service::ImageMetadataService,
        llm_connection_service::LlmConnectionService,
        mcp_service::McpService,
        preset_service::PresetService,
        provider_metadata_service::ProviderMetadataService,
        quick_reply_service::QuickReplyService,
        searxng_search_service::SearxngSearchService,
        secret_service::SecretService,
        settings_service::SettingsService,
        skill_service::SkillService,
        sprite_service::SpriteService,
        theme_service::ThemeService,
        tokenization_service::TokenizationService,
        tts_service::TtsService,
        user_endpoint_access_service::UserEndpointAccessService,
        user_media_service::{ListUserImagesInput, UploadUserImageInput, UserMediaService},
        vector_service::VectorService,
        world_info_service::WorldInfoService,
    },
};
use tt_domain::{
    errors::DomainError,
    ios_policy::{IosPolicyScope, resolve_ios_policy_activation_report},
    models::{agent::AgentChatRef, preset::PresetType, settings::ChatBackupSettings, skill::*},
};
use tt_ports::data_archive::{
    DataArchiveFileGateway, DataRootInitializer, ExportArchiveExecutionRequest,
    ImportArchiveExecutionRequest, UserBackupArchiveExecutionRequest, UserBackupArchiveTarget,
};
use tt_ports::external_import::{DownloadByteLimit, DownloadedBytes, ExternalImportDownloader};
use tt_ports::repositories::agent_workspace_lifecycle_repository::AgentPersistentStatePruneRequest;
use tt_ports::repositories::chat_payload_commit_repository::ChatSwipeSource;
use tt_ports::repositories::chat_repository::ChatByteReader;
use tt_ports::repositories::chat_types::{
    ChatMessageSearchQuery, ChatPayloadCursor, FindLastMessageQuery,
};
use tt_ports::skill_script::SkillScriptEngine;

struct WebHostState {
    host_resources: HostResourceService,
    data_root: PathBuf,
    import_root: PathBuf,
    archive_root: PathBuf,
    channels: Mutex<HashMap<String, mpsc::UnboundedSender<Value>>>,
    chat_byte_readers: Mutex<HashMap<u32, WebChatByteReader>>,
    cold_chat_sources: Mutex<HashMap<u32, Arc<dyn ChatSwipeSource>>>,
    next_chat_byte_reader: AtomicU32,
    dev_logs: Arc<WebDevObservability>,
    web_generations: Arc<web_generation::WebGenerationRegistry>,
    services: WebHostServices,
}

struct WebChatByteReader {
    reader: Box<dyn ChatByteReader>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct OpenColdChatResult {
    source_id: u32,
    reader_id: u32,
}

struct WebHostServices {
    settings: Arc<SettingsService>,
    characters: Arc<CharacterService>,
    chats: Arc<ChatService>,
    group_chats: Arc<GroupChatService>,
    groups: Arc<GroupService>,
    world_info: WorldInfoService,
    presets: PresetService,
    llm_connections: Arc<LlmConnectionService>,
    avatars: AvatarService,
    backgrounds: Arc<BackgroundService>,
    image_metadata: Arc<ImageMetadataService>,
    themes: Arc<ThemeService>,
    quick_replies: Arc<QuickReplyService>,
    sprites: Arc<SpriteService>,
    assets: Arc<AssetService>,
    user_media: Arc<UserMediaService>,
    tokenization: Arc<TokenizationService>,
    provider_metadata: Arc<ProviderMetadataService>,
    searxng_search: Arc<SearxngSearchService>,
    tts: Arc<TtsService>,
    vector: Arc<VectorService>,
    secrets: Arc<SecretService>,
    chat_completion: Arc<ChatCompletionService>,
    chat_commits: ChatPayloadCommitService,
    chat_history: Arc<ChatHistoryCoordinator>,
    endpoint_access: UserEndpointAccessService,
    agent_profiles: Arc<AgentProfileService>,
    agent_profile_diagnostics: Arc<AgentProfileDiagnosticService>,
    prompt_assembly: Arc<tt_application::services::prompt_assembly_service::PromptAssemblyService>,
    agent_runtime: Arc<AgentRuntimeService>,
    agent_run_history: Arc<AgentRunHistoryService>,
    data_archives: Arc<DataArchiveService>,
    database: Arc<DatabaseService>,
    extensions: Arc<ExtensionService>,
    extension_store: Arc<ExtensionStoreService>,
    mcp: Arc<McpService>,
    skills: Arc<SkillService>,
    pending_endpoint_grants: Mutex<HashSet<String>>,
    ios_policy: tt_domain::ios_policy::IosPolicyActivationReport,
}

#[derive(Deserialize)]
struct InvokeRequest {
    command: String,
    #[serde(default)]
    args: Value,
}

#[derive(Deserialize)]
struct ChannelQuery {
    id: String,
}

#[derive(Deserialize)]
struct EventQuery {
    event: String,
}

const MAX_INVOKE_BODY_BYTES: usize = 32 * 1024 * 1024;

#[derive(Deserialize)]
struct FileQuery {
    path: PathBuf,
}

#[derive(Deserialize)]
struct BinaryInvokeQuery {
    command: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillFilesArgs {
    name: String,
    scope: Option<SkillScope>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillImportArgs {
    input: SkillImportInput,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillPreviewArgs {
    input: SkillImportInput,
    target_scope: Option<SkillScope>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillReadArgs {
    name: String,
    path: String,
    scope: Option<SkillScope>,
    start_line: Option<usize>,
    line_count: Option<usize>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillDownloadArgs {
    url: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillDiscardArgs {
    path: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillNameScopeArgs {
    name: String,
    scope: Option<SkillScope>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillWriteArgs {
    name: String,
    path: String,
    content: String,
    scope: Option<SkillScope>,
    expected_sha256: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillInstallArgs {
    request: SkillInstallRequest,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillMoveArgs {
    request: SkillMoveRequest,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillRetargetArgs {
    request: SkillScopeRetargetRequest,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChatSearchArgs {
    query: String,
    character_filter: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChatBackupArgs {
    character_name: String,
    file_name: String,
}

#[derive(Deserialize)]
struct ChatBackupNameArgs {
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RecentChatSummaryArgs {
    character_filter: Option<String>,
    include_metadata: Option<bool>,
    max_entries: Option<usize>,
    pinned: Option<Vec<PinnedCharacterChatDto>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GroupChatSummaryArgs {
    chat_ids: Option<Vec<String>>,
    include_metadata: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RecentGroupChatSummaryArgs {
    chat_ids: Option<Vec<String>>,
    include_metadata: Option<bool>,
    max_entries: Option<usize>,
    pinned: Option<Vec<PinnedGroupChatDto>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GroupChatSearchArgs {
    query: String,
    chat_ids: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChatPayloadTailArgs {
    character_name: String,
    file_name: String,
    max_lines: usize,
    allow_not_found: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChatPayloadBeforeArgs {
    character_name: String,
    file_name: String,
    cursor: ChatPayloadCursor,
    max_lines: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChatPayloadBeforePagesArgs {
    character_name: String,
    file_name: String,
    cursor: ChatPayloadCursor,
    max_lines: usize,
    max_pages: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GroupPayloadTailArgs {
    id: String,
    max_lines: usize,
    allow_not_found: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GroupPayloadBeforeArgs {
    id: String,
    cursor: ChatPayloadCursor,
    max_lines: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GroupPayloadBeforePagesArgs {
    id: String,
    cursor: ChatPayloadCursor,
    max_lines: usize,
    max_pages: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CharacterMessageQueryArgs {
    character_name: String,
    file_name: String,
    query: ChatMessageSearchQuery,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GroupMessageQueryArgs {
    chat_id: String,
    query: ChatMessageSearchQuery,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CharacterFindLastArgs {
    character_name: String,
    file_name: String,
    query: FindLastMessageQuery,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GroupFindLastArgs {
    chat_id: String,
    query: FindLastMessageQuery,
}

#[derive(Deserialize)]
struct SnapshotNameArgs {
    name: String,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct BeginCommitResponse {
    session_id: String,
    max_frame_bytes: u64,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct FinishCommitResponse {
    accepted_size: u64,
    size: u64,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if env::args().nth(1).as_deref() == Some("--healthcheck") {
        return healthcheck();
    }

    let data_root = env::var("TT_DATA_DIR").unwrap_or_else(|_| "/data".to_string());
    tokio::fs::create_dir_all(&data_root).await?;
    let import_root = env::var("TT_IMPORT_DIR").unwrap_or_else(|_| "/imports".to_string());
    let port = env::var("PORT")
        .ok()
        .map(|value| value.parse::<u16>())
        .transpose()?
        .unwrap_or(3093);

    let data_directory = DataDirectory::new(PathBuf::from(&data_root));
    data_directory.initialize().await?;
    let chat_aliases = new_shared_chat_alias_store_for_user_dir(data_directory.default_user());
    let chat_repository = Arc::new(FileChatRepository::with_chat_aliases_and_backup_settings(
        data_directory.characters().to_path_buf(),
        data_directory.chats().to_path_buf(),
        data_directory.group_chats().to_path_buf(),
        data_directory.backups().to_path_buf(),
        chat_aliases.clone(),
        ChatBackupSettings::default(),
    ));
    let character_repository = Arc::new(FileCharacterRepository::with_chat_repository(
        data_directory.characters().to_path_buf(),
        data_directory.chats().to_path_buf(),
        data_directory.default_avatar().to_path_buf(),
        chat_aliases,
        chat_repository.clone(),
    ));
    let world_info_repository = Arc::new(FileWorldInfoRepository::new(
        data_directory.default_user().join("worlds"),
    ));
    let preset_repository: Arc<dyn tt_ports::repositories::preset_repository::PresetRepository> =
        Arc::new(WebPresetRepository::new(
            data_directory.default_user().to_path_buf(),
            PathBuf::from(
                env::var("TT_CONTENT_DIR").unwrap_or_else(|_| "default/content".to_string()),
            ),
        ));
    let agent_repository = Arc::new(FileAgentRepository::new(
        PathBuf::from(&data_root)
            .join("_tauritavern")
            .join("agent-workspaces"),
    ));
    let chat_repository_port: Arc<dyn tt_ports::repositories::chat_repository::ChatRepository> =
        chat_repository.clone();
    let group_chat_repository_port: Arc<
        dyn tt_ports::repositories::group_chat_repository::GroupChatRepository,
    > = chat_repository.clone();
    let history = Arc::new(ChatHistoryCoordinator::new(
        chat_repository_port.clone(),
        group_chat_repository_port.clone(),
    ));
    let chat_payload_commit_repository_port: Arc<
        dyn tt_ports::repositories::chat_payload_commit_repository::ChatPayloadCommitRepository,
    > = chat_repository.clone();
    let chat_commits =
        ChatPayloadCommitService::new(chat_payload_commit_repository_port, history.clone());
    let character_repository_port: Arc<
        dyn tt_ports::repositories::character_repository::CharacterRepository,
    > = character_repository;
    let world_info_repository_port: Arc<
        dyn tt_ports::repositories::world_info_repository::WorldInfoRepository,
    > = world_info_repository;
    let world_info_service = WorldInfoService::new(world_info_repository_port.clone());
    let preset_service = PresetService::new(preset_repository);
    let avatar_repository = Arc::new(FileAvatarRepository::new(
        data_directory.default_user().join("User Avatars"),
    ));
    let avatar_service = AvatarService::new(avatar_repository.clone());
    let background_root = data_directory.default_user().join("backgrounds");
    let image_metadata_repository = Arc::new(FileImageMetadataRepository::new(
        data_directory.default_user().to_path_buf(),
        background_root.clone(),
    ));
    let image_metadata_service =
        Arc::new(ImageMetadataService::new(image_metadata_repository.clone()));
    let background_service = Arc::new(BackgroundService::new(
        Arc::new(FileBackgroundRepository::new(background_root)),
        image_metadata_repository,
    ));
    let theme_service = Arc::new(ThemeService::new(Arc::new(FileThemeRepository::new(
        data_directory.default_user().join("themes"),
    ))));
    let quick_reply_service = Arc::new(QuickReplyService::new(Arc::new(
        FileQuickReplyRepository::new(data_directory.default_user().join("QuickReplies")),
    )));
    let sprite_service = Arc::new(SpriteService::new(Arc::new(FileSpriteRepository::new(
        data_directory.characters().to_path_buf(),
    ))));
    let user_media_service = Arc::new(UserMediaService::new(Arc::new(
        FilesystemUserMediaStore::from_data_root(&data_root),
    )));
    let secret_repository = Arc::new(FileSecretRepository::new(
        data_directory.default_user().join("secrets.json"),
    ));
    let secret_service = Arc::new(SecretService::new(secret_repository.clone(), true));
    let http_clients = Arc::new(HttpClientPool::new("TauriTavern-Web/2.3.0"));
    let ios_policy = resolve_ios_policy_activation_report(IosPolicyScope::Ignored, None)?;
    let provider_metadata_service = Arc::new(ProviderMetadataService::new(
        Arc::new(HttpProviderMetadataRepository::new(http_clients.clone())),
        secret_repository.clone(),
        ios_policy.clone(),
    ));
    let searxng_search_service = Arc::new(SearxngSearchService::new(Arc::new(
        HttpSearxngSearchRepository::new(http_clients.clone()),
    )));
    let tts_service = Arc::new(TtsService::new(
        Arc::new(HttpTtsRepository::new(http_clients.clone())),
        secret_repository.clone(),
    ));
    let tokenization_service = Arc::new(TokenizationService::new(Arc::new(
        MiktikTokenizerRepository::new(
            PathBuf::from(&data_root).join("_cache").join("tokenizers"),
            http_clients.clone(),
        ),
    )));
    let vector_service = Arc::new(VectorService::new(
        Arc::new(RedbVectorRepository::new(
            data_directory
                .default_user()
                .join("vectors")
                .join("tauritavern-v1.redb"),
        )),
        Arc::new(HttpEmbeddingRepository::new(http_clients.clone())),
        Arc::new(CandleLocalEmbeddingRepository::new(
            PathBuf::from(&data_root)
                .join("_cache")
                .join("embedding-models"),
        )),
        secret_repository.clone(),
        ios_policy.clone(),
    ));
    let asset_service = Arc::new(AssetService::new(
        Arc::new(FileAssetRepository::new(
            data_directory.default_user().to_path_buf(),
            data_directory.default_user().join("assets"),
            data_directory.default_user().join("characters"),
        )),
        Arc::new(WebExternalImportDownloader::new(http_clients.clone())),
    ));
    let extensions = Arc::new(ExtensionService::new(
        Arc::new(FileExtensionRepository::new(
            data_directory.default_user().join("extensions"),
            data_directory.global_extensions().to_path_buf(),
            data_directory.extension_sources().to_path_buf(),
            http_clients.clone(),
        )),
        Arc::new(Semaphore::new(1)),
    ));
    let extension_store = Arc::new(ExtensionStoreService::new(Arc::new(
        FileExtensionStoreRepository::new(
            PathBuf::from(&data_root)
                .join("_tauritavern")
                .join("extension-store"),
        ),
    )));
    let settings_repository = Arc::new(FileSettingsRepository::new(
        data_directory.settings().to_path_buf(),
        serde_json::from_str(include_str!("../../../default/content/settings.json"))?,
    ));
    let settings_repository_port: Arc<
        dyn tt_ports::repositories::settings_repository::SettingsRepository,
    > = settings_repository.clone();
    let settings_service = Arc::new(SettingsService::new(
        avatar_repository.clone(),
        settings_repository_port.clone(),
        http_clients.clone(),
        chat_repository.clone(),
    ));
    let llm_connection_service = Arc::new(LlmConnectionService::new(
        Arc::new(FileLlmConnectionRepository::new(
            PathBuf::from(&data_root)
                .join("_tauritavern")
                .join("llm-connections"),
        )),
        settings_repository_port.clone(),
    ));
    settings_service.reload().await?;
    let dev_logs = WebDevObservability::new(PathBuf::from(&data_root));
    if let Ok(settings) = settings_service.get_tauritavern_settings().await {
        dev_logs.set_llm_keep(settings.dev.llm_api_keep as usize);
    }
    let chat_completion_repository = Arc::new(LoggingChatCompletionRepository::new(
        Arc::new(HttpChatCompletionRepository::new(http_clients.clone())),
        dev_logs.clone(),
    ));
    let chat_completion_repository_port: Arc<
        dyn tt_ports::repositories::chat_completion_repository::ChatCompletionRepository,
    > = chat_completion_repository;
    let prompt_cache_repository = Arc::new(FilePromptCacheRepository::new(
        PathBuf::from(&data_root)
            .join("_tauritavern")
            .join("prompt-cache"),
    ));
    let prompt_cache_repository_port: Arc<
        dyn tt_ports::repositories::prompt_cache_repository::PromptCacheRepository,
    > = prompt_cache_repository;
    let chat_completion = Arc::new(ChatCompletionService::new(
        chat_completion_repository_port,
        secret_repository.clone(),
        settings_repository_port.clone(),
        prompt_cache_repository_port,
        None,
        ios_policy.clone(),
    ));
    let agent_profile_file_repository = Arc::new(FileAgentProfileRepository::new(
        PathBuf::from(&data_root)
            .join("_tauritavern")
            .join("agent-profiles"),
    ));
    let agent_profile_repository: Arc<
        dyn tt_ports::repositories::agent_profile_repository::AgentProfileRepository,
    > = agent_profile_file_repository.clone();
    let agent_profile_health_repository: Arc<
        dyn tt_ports::repositories::agent_profile_storage_health_repository::AgentProfileStorageHealthRepository,
    > = agent_profile_file_repository;
    let agent_preset_repository: Arc<
        dyn tt_ports::repositories::preset_repository::PresetRepository,
    > = Arc::new(WebPresetRepository::new(
        data_directory.default_user().to_path_buf(),
        PathBuf::from(env::var("TT_CONTENT_DIR").unwrap_or_else(|_| "default/content".to_string())),
    ));
    let agent_profile_service = Arc::new(AgentProfileService::new(
        agent_profile_repository,
        agent_profile_health_repository,
        agent_preset_repository.clone(),
    ));
    let agent_profile_diagnostics = Arc::new(AgentProfileDiagnosticService::new(
        agent_profile_service.clone(),
        agent_preset_repository.clone(),
        llm_connection_service.clone(),
    ));
    let skill_service = Arc::new(SkillService::with_external_import_downloader(
        Arc::new(FileSkillRepository::new(
            PathBuf::from(&data_root)
                .join("_tauritavern")
                .join("skills"),
        )),
        Arc::new(WebExternalImportDownloader::new(http_clients.clone())),
    ));
    let mcp_service = Arc::new(McpService::new(
        Arc::new(tt_adapter_storage_core::FileMcpServerRepository::new(
            PathBuf::from(&data_root).join("_tauritavern").join("mcp"),
        )),
        Arc::new(RmcpMcpGateway::new(http_clients.clone())),
    ));
    let skill_script_engine: Arc<dyn SkillScriptEngine> = Arc::new(QuickJsScriptEngine::new());
    let prompt_assembly_service = Arc::new(
        tt_application::services::prompt_assembly_service::PromptAssemblyService::new(
            agent_profile_service.clone(),
            agent_preset_repository.clone(),
            llm_connection_service.clone(),
        ),
    );
    let agent_runtime = Arc::new(AgentRuntimeService::new(
        agent_repository.clone(),
        agent_repository.clone(),
        agent_repository.clone(),
        chat_repository_port.clone(),
        group_chat_repository_port.clone(),
        skill_service.clone(),
        Arc::new(ChatCompletionAgentModelGateway::new(
            chat_completion.clone(),
        )),
        agent_profile_service.clone(),
        llm_connection_service.clone(),
        prompt_assembly_service.clone(),
        mcp_service.clone(),
        skill_script_engine,
    ));
    let agent_activity: Arc<dyn AgentRunActivity> = agent_runtime.clone();
    let lifecycle = Arc::new(AgentWorkspaceLifecycleService::new(
        agent_repository.clone(),
        agent_activity.clone(),
        agent_runtime.run_lifecycle_lock(),
    ));
    let agent_run_history = Arc::new(AgentRunHistoryService::new(
        agent_repository.clone(),
        settings_repository_port.clone(),
        agent_activity,
        agent_runtime.run_lifecycle_lock(),
    ));
    let group_chat_service = Arc::new(GroupChatService::new(
        group_chat_repository_port,
        lifecycle.clone(),
        history.clone(),
    ));
    let character_service = Arc::new(CharacterService::new(
        character_repository_port.clone(),
        chat_repository_port.clone(),
        world_info_repository_port,
        lifecycle.clone(),
        history.clone(),
    ));
    let chat_service = Arc::new(ChatService::new(
        chat_repository_port.clone(),
        character_repository_port.clone(),
        lifecycle.clone(),
        history.clone(),
    ));
    let group_service = Arc::new(GroupService::new(
        Arc::new(FileGroupRepository::new(
            data_directory.groups().to_path_buf(),
            data_directory.group_chats().to_path_buf(),
        )),
        lifecycle.clone(),
        history.clone(),
    ));
    let endpoint_access = UserEndpointAccessService::initialize(
        Arc::new(FileUserEndpointGrantRepository::new(
            PathBuf::from(&data_root)
                .join("_tauritavern")
                .join("security")
                .join("user-endpoint-grants.json"),
        )),
        http_clients.clone(),
    )
    .await;
    let archive_root = env::temp_dir().join(format!(
        "tauritavern-web-archives-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&archive_root)?;
    let database_service = Arc::new(DatabaseService::new(Arc::new(TriviumDatabaseBackend::new(
        PathBuf::from(&data_root)
            .join("_tauritavern")
            .join("databases"),
    ))));
    let data_archives = Arc::new(DataArchiveService::new(
        database_service.clone(),
        Arc::new(DataArchiveJobRegistry::new()),
        tokio::runtime::Handle::current(),
        Arc::new(FileDataArchiveExecutor {
            prepare_personas: tt_adapter_media::persona_cards::migrate_personas,
            read_personas: tt_adapter_media::persona_cards::read_personas,
        }),
        Arc::new(WebDataArchiveFileGateway::new(
            PathBuf::from(&data_root),
            archive_root.clone(),
        )),
        Arc::new(WebDataRootInitializer),
        Arc::new(WebDataChangeReconciler {
            settings: settings_service.clone(),
            characters: character_service.clone(),
            chats: chat_service.clone(),
            groups: group_service.clone(),
            group_chats: group_chat_service.clone(),
            secrets: secret_service.clone(),
            chat_history: history.clone(),
        }),
    ));
    let web_generations = web_generation::WebGenerationRegistry::load(
        PathBuf::from(&data_root)
            .join("_tauritavern")
            .join("web-generations"),
        chat_completion.clone(),
    )
    .await
    .map_err(std::io::Error::other)?;
    let state = Arc::new(WebHostState {
        host_resources: HostResourceService::new(
            false,
            Arc::new(FilesystemHostResourceStore::from_data_root(&data_root)),
        ),
        data_root: PathBuf::from(&data_root),
        import_root: PathBuf::from(&import_root),
        archive_root,
        channels: Mutex::new(HashMap::new()),
        chat_byte_readers: Mutex::new(HashMap::new()),
        cold_chat_sources: Mutex::new(HashMap::new()),
        next_chat_byte_reader: AtomicU32::new(1),
        dev_logs,
        web_generations,
        services: WebHostServices {
            settings: settings_service,
            characters: character_service,
            chats: chat_service,
            group_chats: group_chat_service,
            groups: group_service,
            world_info: world_info_service,
            presets: preset_service,
            llm_connections: llm_connection_service,
            avatars: avatar_service,
            backgrounds: background_service,
            image_metadata: image_metadata_service,
            themes: theme_service,
            quick_replies: quick_reply_service,
            sprites: sprite_service,
            assets: asset_service,
            user_media: user_media_service,
            tokenization: tokenization_service,
            provider_metadata: provider_metadata_service,
            searxng_search: searxng_search_service,
            tts: tts_service,
            vector: vector_service,
            secrets: secret_service,
            chat_completion,
            chat_commits,
            chat_history: history,
            endpoint_access,
            agent_profiles: agent_profile_service,
            agent_profile_diagnostics,
            prompt_assembly: prompt_assembly_service,
            agent_runtime,
            agent_run_history,
            data_archives,
            database: database_service,
            extensions,
            extension_store,
            mcp: mcp_service,
            skills: skill_service,
            pending_endpoint_grants: Mutex::new(HashSet::new()),
            ios_policy,
        },
    });
    let router = Router::new()
        .route("/healthz", get(health))
        .route("/__tt/ready", get(ready))
        .route(
            "/__tt/invoke",
            post(invoke).layer(DefaultBodyLimit::max(MAX_INVOKE_BODY_BYTES)),
        )
        .route("/__tt/invoke-binary", post(invoke_binary))
        .route("/__tt/events", get(events_socket))
        .route("/__tt/channels", get(channel_socket))
        .route("/__tt/file", get(read_data_file))
        .route("/__tt/uploads", post(stage_upload).delete(discard_upload))
        .route("/api/bootstrap", post(api_bootstrap))
        .fallback(serve_host_resource)
        .with_state(state);
    let address = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(address).await?;
    println!("TauriTavern web host listening on {address}");
    axum::serve(listener, router).await?;
    Ok(())
}

async fn read_data_file(
    State(state): State<Arc<WebHostState>>,
    Query(query): Query<FileQuery>,
) -> Response<Body> {
    let canonical_root = match tokio::fs::canonicalize(&state.data_root).await {
        Ok(path) => path,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let canonical_archive_root = match tokio::fs::canonicalize(&state.archive_root).await {
        Ok(path) => path,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let canonical_file = match tokio::fs::canonicalize(&query.path).await {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return StatusCode::NOT_FOUND.into_response();
        }
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    if !canonical_file.starts_with(&canonical_root)
        && !canonical_file.starts_with(&canonical_archive_root)
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let metadata = match tokio::fs::metadata(&canonical_file).await {
        Ok(metadata) if metadata.is_file() => metadata,
        Ok(_) => return StatusCode::BAD_REQUEST.into_response(),
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    let bytes = match tokio::fs::read(&canonical_file).await {
        Ok(bytes) => bytes,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/octet-stream")
        .header("content-length", metadata.len())
        .body(Body::from(bytes))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

#[derive(Deserialize)]
struct UploadQuery {
    extension: String,
    #[serde(default)]
    kind: Option<String>,
}

async fn stage_upload(
    State(state): State<Arc<WebHostState>>,
    Query(query): Query<UploadQuery>,
    request: Request<Body>,
) -> Response<Body> {
    const MAX_UPLOAD_BYTES: u64 = 1024 * 1024 * 1024;
    let extension = query
        .extension
        .trim()
        .trim_start_matches('.')
        .to_ascii_lowercase();
    if extension.is_empty()
        || extension.len() > 12
        || !extension.chars().all(|ch| ch.is_ascii_alphanumeric())
    {
        return bad_request("Invalid upload file extension".to_string());
    }
    if query.kind.as_deref().is_some_and(|kind| {
        kind.len() > 40
            || !kind
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
    }) {
        return bad_request("Invalid upload kind".to_string());
    }
    let staging_root = state.data_root.join("_tauritavern").join("web-uploads");
    if let Err(error) = tokio::fs::create_dir_all(&staging_root).await {
        return application_error_message(error.to_string());
    }
    let path = staging_root.join(format!("{}.{}", uuid::Uuid::new_v4(), extension));
    let mut file = match tokio::fs::File::create(&path).await {
        Ok(file) => file,
        Err(error) => return application_error_message(error.to_string()),
    };
    let mut size = 0u64;
    let mut stream = request.into_body().into_data_stream();
    loop {
        let chunk = match stream.try_next().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(error) => {
                let _ = tokio::fs::remove_file(&path).await;
                return bad_request(format!("Upload stream failed: {error}"));
            }
        };
        size = match size.checked_add(chunk.len() as u64) {
            Some(size) if size <= MAX_UPLOAD_BYTES => size,
            _ => {
                let _ = tokio::fs::remove_file(&path).await;
                return bad_request("Upload exceeds the 1 GiB limit".to_string());
            }
        };
        if let Err(error) = tokio::io::AsyncWriteExt::write_all(&mut file, &chunk).await {
            let _ = tokio::fs::remove_file(&path).await;
            return application_error_message(error.to_string());
        }
    }
    if size == 0 {
        let _ = tokio::fs::remove_file(&path).await;
        return bad_request("Uploaded file is empty".to_string());
    }
    if let Err(error) = tokio::io::AsyncWriteExt::flush(&mut file).await {
        let _ = tokio::fs::remove_file(&path).await;
        return application_error_message(error.to_string());
    }
    (
        StatusCode::OK,
        axum::Json(json!({ "file_path": path.to_string_lossy() })),
    )
        .into_response()
}

async fn discard_upload(
    State(state): State<Arc<WebHostState>>,
    Query(query): Query<FileQuery>,
) -> Response<Body> {
    let staging_root = state.data_root.join("_tauritavern").join("web-uploads");
    let (root, path) = match tokio::try_join!(
        tokio::fs::canonicalize(staging_root),
        tokio::fs::canonicalize(query.path),
    ) {
        Ok((root, path)) => (root, path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return StatusCode::NO_CONTENT.into_response();
        }
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    if !path.starts_with(root) || !path.is_file() {
        return StatusCode::FORBIDDEN.into_response();
    }
    match tokio::fs::remove_file(path).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn events_socket(
    State(state): State<Arc<WebHostState>>,
    Query(query): Query<EventQuery>,
    ws: WebSocketUpgrade,
) -> Response<Body> {
    let Some(receiver) = state.dev_logs.subscribe(&query.event) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    ws.on_upgrade(move |socket| serve_event_socket(receiver, socket))
        .into_response()
}

async fn serve_event_socket(
    mut receiver: tokio::sync::broadcast::Receiver<Value>,
    mut socket: WebSocket,
) {
    loop {
        tokio::select! {
            event = receiver.recv() => {
                let Ok(payload) = event else { continue };
                let text = json!({ "payload": payload }).to_string();
                if socket.send(axum::extract::ws::Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(axum::extract::ws::Message::Close(_))) | None | Some(Err(_)) => break,
                    _ => {}
                }
            }
        }
    }
}

async fn channel_socket(
    State(state): State<Arc<WebHostState>>,
    Query(query): Query<ChannelQuery>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    if query.id.is_empty() || query.id.len() > 128 {
        return StatusCode::BAD_REQUEST.into_response();
    }
    ws.on_upgrade(move |socket| serve_channel_socket(state, query.id, socket))
        .into_response()
}

async fn serve_channel_socket(state: Arc<WebHostState>, id: String, mut socket: WebSocket) {
    let (sender, mut receiver) = mpsc::unbounded_channel::<Value>();
    state.channels.lock().await.insert(id.clone(), sender);
    loop {
        tokio::select! {
            outgoing = receiver.recv() => {
                let Some(payload) = outgoing else { break };
                let text = json!({ "type": "message", "payload": payload }).to_string();
                if socket.send(axum::extract::ws::Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(axum::extract::ws::Message::Close(_))) | None | Some(Err(_)) => break,
                    _ => {}
                }
            }
        }
    }
    state.channels.lock().await.remove(&id);
}

fn healthcheck() -> Result<(), Box<dyn std::error::Error>> {
    let port = env::var("PORT")
        .ok()
        .map(|value| value.parse::<u16>())
        .transpose()?
        .unwrap_or(3093);
    let mut stream = TcpStream::connect_timeout(
        &SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_secs(2),
    )?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    if response.starts_with("HTTP/1.1 200") || response.starts_with("HTTP/1.0 200") {
        Ok(())
    } else {
        Err(format!("web host health check failed: {response}").into())
    }
}

async fn health() -> impl IntoResponse {
    (
        StatusCode::OK,
        axum::Json(json!({ "status": "ok", "service": "resource-host" })),
    )
}

async fn ready() -> impl IntoResponse {
    (StatusCode::OK, axum::Json(json!({ "ready": true })))
}

async fn api_bootstrap(State(state): State<Arc<WebHostState>>) -> Response<Body> {
    match build_bootstrap_snapshot(&state).await {
        Ok(snapshot) => (StatusCode::OK, axum::Json(json!(snapshot))).into_response(),
        Err(error) => application_error(error),
    }
}

async fn invoke(
    State(state): State<Arc<WebHostState>>,
    Json(request): Json<InvokeRequest>,
) -> Response<Body> {
    let state_for_logs = state.clone();
    let result: Result<Value, String> = match request.command.as_str() {
        "devlog_append_frontend_logs" => {
            let entries = request
                .args
                .get("entries")
                .cloned()
                .ok_or_else(|| "Missing frontend log entries".to_string())
                .and_then(|value| {
                    serde_json::from_value::<Vec<FrontendLogInput>>(value)
                        .map_err(|error| format!("Invalid frontend log entries: {error}"))
                });
            match entries {
                Ok(entries) => {
                    state.dev_logs.append_frontend(entries);
                    Ok(Value::Null)
                }
                Err(error) => Err(error),
            }
        }
        "devlog_set_backend_log_stream_enabled" => {
            state.dev_logs.set_backend_stream_enabled(
                request
                    .args
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            );
            Ok(Value::Null)
        }
        "devlog_get_backend_log_tail" => {
            let limit = request
                .args
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(800)
                .clamp(1, 800) as usize;
            serde_json::to_value(state.dev_logs.backend_tail(limit))
                .map_err(|error| error.to_string())
        }
        "devlog_set_llm_api_log_stream_enabled" => {
            state.dev_logs.set_llm_stream_enabled(
                request
                    .args
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            );
            Ok(Value::Null)
        }
        "devlog_get_llm_api_log_index" => {
            let limit = request
                .args
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(50)
                .clamp(1, 100) as usize;
            serde_json::to_value(state.dev_logs.llm_index(limit)).map_err(|error| error.to_string())
        }
        "devlog_get_llm_api_log_preview" => {
            let id = request.args.get("id").and_then(Value::as_u64).unwrap_or(0);
            state
                .dev_logs
                .llm_preview(id)
                .map(|value| json!(value))
                .ok_or_else(|| format!("LLM API log {id} was not found"))
        }
        "devlog_get_llm_api_log_raw" => {
            let id = request.args.get("id").and_then(Value::as_u64).unwrap_or(0);
            state
                .dev_logs
                .llm_raw(id)
                .map(|value| json!(value))
                .ok_or_else(|| format!("LLM API log {id} was not found"))
        }
        "devlog_export_bundle" => {
            let frontend = request
                .args
                .get("frontend_entries")
                .or_else(|| request.args.get("frontendEntries"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let backend = state.dev_logs.backend_tail(800);
            state
                .dev_logs
                .export_bundle(frontend, backend)
                .await
                .map(|path| json!(path.to_string_lossy().to_string()))
        }
        "sanitize_filename" => {
            let name = string_arg(&request.args, "file_name", "fileName");
            if name.is_empty() {
                Err("No fileName specified".to_string())
            } else {
                Ok(json!(tt_domain::models::filename::sanitize_filename(name)))
            }
        }
        "get_personas" => state
            .services
            .avatars
            .get_personas()
            .await
            .map_err(|error| error.to_string())
            .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string())),
        "get_avatars" => state
            .services
            .avatars
            .get_avatars()
            .await
            .map_err(|error| error.to_string())
            .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string())),
        "get_assets_library" => state
            .services
            .assets
            .list_assets()
            .await
            .map_err(|error| error.to_string())
            .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string())),
        "delete_asset" => state
            .services
            .assets
            .delete_asset_file(
                string_arg(&request.args, "category", "category"),
                string_arg(&request.args, "filename", "filename"),
            )
            .await
            .map(|()| Value::Null)
            .map_err(|error| error.to_string()),
        "get_character_assets" => state
            .services
            .assets
            .list_character_assets(
                string_arg(&request.args, "name", "name"),
                string_arg(&request.args, "category", "category"),
            )
            .await
            .map_err(|error| error.to_string())
            .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string())),
        "download_asset" => state
            .services
            .assets
            .download_asset(
                string_arg(&request.args, "url", "url"),
                string_arg(&request.args, "category", "category"),
                string_arg(&request.args, "filename", "filename"),
            )
            .await
            .map_err(|error| error.to_string())
            .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string())),
        "download_external_import_url" => {
            let url = string_arg(&request.args, "url", "url");
            let filename = url::Url::parse(url)
                .ok()
                .and_then(|parsed| {
                    parsed
                        .path_segments()
                        .and_then(Iterator::last)
                        .map(str::to_owned)
                })
                .filter(|name| !name.is_empty())
                .map(|name| tt_domain::models::filename::sanitize_filename(&name))
                .filter(|name| {
                    !name.is_empty()
                        && name
                            .chars()
                            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
                })
                .unwrap_or_else(|| "external_import.png".to_string());
            state
                .services
                .assets
                .download_asset(url, "character", &filename)
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| {
                    if !result.mime_type.eq_ignore_ascii_case("image/png") && !filename.to_ascii_lowercase().ends_with(".png") {
                        return Err("Only PNG imports are supported".to_string());
                    }
                    serde_json::to_value(json!({
                        "data": result.data,
                        "fileName": if filename.to_ascii_lowercase().ends_with(".png") { filename } else { format!("{filename}.png") },
                        "mimeType": "image/png",
                    })).map_err(|error| error.to_string())
                })
        }
        "upload_user_image" => {
            let input = UploadUserImageInput {
                image_base64: string_arg(&request.args, "image_base64", "imageBase64").to_string(),
                format: string_arg(&request.args, "format", "format").to_string(),
                filename: request
                    .args
                    .get("filename")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                ch_name: request
                    .args
                    .get("ch_name")
                    .or_else(|| request.args.get("chName"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            };
            state
                .services
                .user_media
                .upload_user_image(input)
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string()))
        }
        "list_user_images" => {
            let input = ListUserImagesInput {
                folder: string_arg(&request.args, "folder", "folder").to_string(),
                sort_field: request
                    .args
                    .get("sort_field")
                    .or_else(|| request.args.get("sortField"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                sort_order: request
                    .args
                    .get("sort_order")
                    .or_else(|| request.args.get("sortOrder"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                media_type: request
                    .args
                    .get("media_type")
                    .or_else(|| request.args.get("mediaType"))
                    .and_then(Value::as_u64)
                    .and_then(|value| u32::try_from(value).ok()),
            };
            state
                .services
                .user_media
                .list_user_images(input)
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string()))
        }
        "list_user_image_folders" => state
            .services
            .user_media
            .list_user_image_folders()
            .await
            .map_err(|error| error.to_string())
            .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string())),
        "delete_user_image" => state
            .services
            .user_media
            .delete_user_image(string_arg(&request.args, "path", "path"))
            .await
            .map(|()| Value::Null)
            .map_err(|error| error.to_string()),
        "upload_user_file" => {
            let name = string_arg(&request.args, "name", "name");
            match parse_byte_array(&request.args, "data_base64", "dataBase64") {
                Ok(bytes) => match save_user_file(&state, name, bytes).await {
                    Ok(path) => Ok(json!({ "path": path })),
                    Err(error) => Err(error),
                },
                Err(error) => Err(error),
            }
        }
        "delete_user_file" => {
            match resolve_user_file(&state, string_arg(&request.args, "path", "path")).await {
                Ok(path) => match tokio::fs::remove_file(path).await {
                    Ok(()) => Ok(Value::Null),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        Err("File not found".to_string())
                    }
                    Err(error) => Err(error.to_string()),
                },
                Err(error) => Err(error),
            }
        }
        "verify_user_files" => {
            let urls = request
                .args
                .get("urls")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let mut result = serde_json::Map::new();
            for url in urls
                .into_iter()
                .filter_map(|value| value.as_str().map(str::to_owned))
            {
                let exists = resolve_user_file(&state, &url)
                    .await
                    .ok()
                    .is_some_and(|path| path.is_file());
                result.insert(url, Value::Bool(exists));
            }
            Ok(Value::Object(result))
        }
        "count_openai_tokens_batch" => {
            match parse_dto::<OpenAiTokenCountBatchRequestDto>(&request.args, "dto") {
                Ok(dto) => state
                    .services
                    .tokenization
                    .count_openai_tokens_batch(dto)
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|value| {
                        serde_json::to_value(value).map_err(|error| error.to_string())
                    }),
                Err(error) => Err(error),
            }
        }
        "count_openai_token_prefixes" => {
            match parse_dto::<OpenAiTokenPrefixCountRequestDto>(&request.args, "dto") {
                Ok(dto) => state
                    .services
                    .tokenization
                    .count_openai_token_prefixes(dto)
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|value| {
                        serde_json::to_value(value).map_err(|error| error.to_string())
                    }),
                Err(error) => Err(error),
            }
        }
        "encode_tokens" => match parse_dto::<EncodeTokensRequestDto>(&request.args, "dto") {
            Ok(dto) => state
                .services
                .tokenization
                .encode_tokens(dto)
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string())),
            Err(error) => Err(error),
        },
        "decode_tokens" => match parse_dto::<DecodeTokensRequestDto>(&request.args, "dto") {
            Ok(dto) => state
                .services
                .tokenization
                .decode_tokens(dto)
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string())),
            Err(error) => Err(error),
        },
        "build_openai_logit_bias" => {
            match parse_dto::<OpenAiLogitBiasRequestDto>(&request.args, "dto") {
                Ok(dto) => state
                    .services
                    .tokenization
                    .build_openai_logit_bias(dto)
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|value| {
                        serde_json::to_value(value).map_err(|error| error.to_string())
                    }),
                Err(error) => Err(error),
            }
        }
        "get_openrouter_model_providers" => {
            match parse_dto::<ProviderModelProvidersRequestDto>(&request.args, "dto") {
                Ok(dto) => state
                    .services
                    .provider_metadata
                    .openrouter_model_providers(dto)
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|value| {
                        serde_json::to_value(value).map_err(|error| error.to_string())
                    }),
                Err(error) => Err(error),
            }
        }
        "get_openrouter_credits" => state
            .services
            .provider_metadata
            .openrouter_credits()
            .await
            .map_err(|error| error.to_string())
            .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string())),
        "get_nanogpt_model_providers" => {
            match parse_dto::<ProviderModelProvidersRequestDto>(&request.args, "dto") {
                Ok(dto) => state
                    .services
                    .provider_metadata
                    .nanogpt_model_providers(dto)
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|value| {
                        serde_json::to_value(value).map_err(|error| error.to_string())
                    }),
                Err(error) => Err(error),
            }
        }
        "get_nanogpt_credits" => state
            .services
            .provider_metadata
            .nanogpt_credits()
            .await
            .map_err(|error| error.to_string())
            .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string())),
        "get_siliconflow_embedding_models" => {
            match parse_dto::<SiliconFlowEmbeddingModelsRequestDto>(&request.args, "dto") {
                Ok(dto) => state
                    .services
                    .provider_metadata
                    .siliconflow_embedding_models(dto)
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|value| {
                        serde_json::to_value(value).map_err(|error| error.to_string())
                    }),
                Err(error) => Err(error),
            }
        }
        "get_workers_ai_embedding_models" => {
            match parse_dto::<WorkersAiModelsRequestDto>(&request.args, "dto") {
                Ok(dto) => state
                    .services
                    .provider_metadata
                    .workers_ai_embedding_models(dto)
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|value| {
                        serde_json::to_value(value).map_err(|error| error.to_string())
                    }),
                Err(error) => Err(error),
            }
        }
        "get_workers_ai_multimodal_models" => {
            match parse_dto::<WorkersAiModelsRequestDto>(&request.args, "dto") {
                Ok(dto) => state
                    .services
                    .provider_metadata
                    .workers_ai_multimodal_models(dto)
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|value| {
                        serde_json::to_value(value).map_err(|error| error.to_string())
                    }),
                Err(error) => Err(error),
            }
        }
        "search_searxng" => match parse_dto::<SearxngSearchRequestDto>(&request.args, "dto") {
            Ok(dto) => match state.services.searxng_search.prepare_request(dto) {
                Ok(request) => state
                    .services
                    .searxng_search
                    .search(request)
                    .await
                    .map(Value::String)
                    .map_err(|error| error.to_string()),
                Err(error) => Err(error.to_string()),
            },
            Err(error) => Err(error),
        },
        "tts_handle" => state
            .services
            .tts
            .handle_request(
                string_arg(&request.args, "path", "path").to_string(),
                request.args.get("body").cloned().unwrap_or(Value::Null),
            )
            .await
            .map_err(|error| error.to_string())
            .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string())),
        "vector_handle" => {
            let path = string_arg(&request.args, "path", "path");
            let body = request.args.get("body").cloned().unwrap_or(Value::Null);
            match serde_json::from_value::<VectorRouteRequestDto>(body) {
                Ok(dto) => state
                    .services
                    .vector
                    .handle_request(path, dto)
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|value| {
                        serde_json::to_value(value).map_err(|error| error.to_string())
                    }),
                Err(error) => Err(format!("Invalid vector request body: {error}")),
            }
        }
        "delete_avatar" => state
            .services
            .avatars
            .delete_avatar(string_arg(&request.args, "avatar", "avatar"))
            .await
            .map(|()| Value::Null)
            .map_err(|error| error.to_string()),
        "upload_avatar" => {
            let path = string_arg(&request.args, "file_path", "filePath");
            match resolve_staged_upload(&state, path).await {
                Ok(path) => {
                    let overwrite = request
                        .args
                        .get("overwrite_name")
                        .or_else(|| request.args.get("overwriteName"))
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    let crop = request
                        .args
                        .get("crop")
                        .and_then(Value::as_str)
                        .map(serde_json::from_str::<tt_domain::models::avatar::CropInfo>)
                        .transpose()
                        .map_err(|error| format!("Invalid avatar crop information: {error}"));
                    match crop {
                        Ok(crop) => state
                            .services
                            .avatars
                            .upload_avatar(&path, overwrite, crop)
                            .await
                            .map_err(|error| error.to_string())
                            .and_then(|value| {
                                serde_json::to_value(value).map_err(|error| error.to_string())
                            }),
                        Err(error) => Err(error),
                    }
                }
                Err(error) => Err(error),
            }
        }
        "update_avatar" => match parse_dto::<UpdateAvatarDto>(&request.args, "dto") {
            Ok(mut dto) => match resolve_staged_upload(&state, &dto.avatar_path).await {
                Ok(path) => {
                    dto.avatar_path = path.to_string_lossy().into_owned();
                    state
                        .services
                        .characters
                        .update_avatar(dto)
                        .await
                        .map(|()| Value::Null)
                        .map_err(|error| error.to_string())
                }
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        },
        "get_all_backgrounds" => state
            .services
            .image_metadata
            .get_background_list_entries()
            .await
            .map_err(|error| error.to_string())
            .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string())),
        "get_all_background_metadata" => {
            let prefix = request.args.get("prefix").and_then(Value::as_str);
            state
                .services
                .image_metadata
                .get_all_background_metadata(prefix)
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string()))
        }
        "get_background_folders" => state
            .services
            .image_metadata
            .get_background_folders()
            .await
            .map_err(|error| error.to_string())
            .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string())),
        "create_image_metadata_folder" => {
            invoke_dto(
                &request.args,
                |dto: CreateImageMetadataFolderDto| async move {
                    state.services.image_metadata.create_folder(dto).await
                },
            )
            .await
        }
        "update_image_metadata_folder" => {
            invoke_dto(
                &request.args,
                |dto: UpdateImageMetadataFolderDto| async move {
                    state.services.image_metadata.update_folder(dto).await
                },
            )
            .await
        }
        "delete_image_metadata_folder" => {
            invoke_dto(
                &request.args,
                |dto: DeleteImageMetadataFolderDto| async move {
                    state
                        .services
                        .image_metadata
                        .delete_folder(dto)
                        .await
                        .map(|()| Value::Null)
                },
            )
            .await
        }
        "set_image_metadata_folder_thumbnails" => {
            invoke_dto(
                &request.args,
                |dto: SetImageMetadataFolderThumbnailsDto| async move {
                    state
                        .services
                        .image_metadata
                        .set_folder_thumbnails(dto)
                        .await
                        .map(|()| Value::Null)
                },
            )
            .await
        }
        "assign_images_to_metadata_folder" => {
            invoke_dto(
                &request.args,
                |dto: ImageMetadataFolderAssignmentDto| async move {
                    state
                        .services
                        .image_metadata
                        .assign_images_to_folder(dto)
                        .await
                        .map(|()| Value::Null)
                },
            )
            .await
        }
        "unassign_images_from_metadata_folder" => {
            invoke_dto(
                &request.args,
                |dto: ImageMetadataFolderAssignmentDto| async move {
                    state
                        .services
                        .image_metadata
                        .unassign_images_from_folder(dto)
                        .await
                        .map(|()| Value::Null)
                },
            )
            .await
        }
        "delete_background" => {
            invoke_dto(&request.args, |dto: DeleteBackgroundDto| async move {
                state
                    .services
                    .backgrounds
                    .delete_background(&dto.bg)
                    .await
                    .map(|()| Value::Null)
                    .map_err(ApplicationError::from)
            })
            .await
        }
        "rename_background" => {
            invoke_dto(&request.args, |dto: RenameBackgroundDto| async move {
                state
                    .services
                    .backgrounds
                    .rename_background(&dto.old_bg, &dto.new_bg)
                    .await
                    .map(|()| Value::Null)
                    .map_err(ApplicationError::from)
            })
            .await
        }
        "upload_background_from_path" => {
            let filename = string_arg(&request.args, "filename", "filename");
            let path = string_arg(&request.args, "file_path", "filePath");
            match resolve_staged_upload(&state, path).await {
                Ok(path) => state
                    .services
                    .backgrounds
                    .upload_background_from_path(filename, &path)
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|value| {
                        serde_json::to_value(value).map_err(|error| error.to_string())
                    }),
                Err(error) => Err(error),
            }
        }
        "upload_background" => {
            let filename = string_arg(&request.args, "filename", "filename");
            match parse_byte_array(&request.args, "data", "data") {
                Ok(bytes) => state
                    .services
                    .backgrounds
                    .upload_background(filename, &bytes)
                    .await
                    .map(Value::String)
                    .map_err(|error| error.to_string()),
                Err(error) => Err(error),
            }
        }
        "save_theme" => {
            let dto = request.args.get("dto").cloned().unwrap_or(Value::Null);
            let name = dto.get("name").and_then(Value::as_str).unwrap_or_default();
            let data = dto.get("data").cloned().unwrap_or(Value::Null);
            state
                .services
                .themes
                .save_theme(name, data)
                .await
                .map(|()| Value::Null)
                .map_err(|error| error.to_string())
        }
        "delete_theme" => {
            let dto = request.args.get("dto").cloned().unwrap_or(Value::Null);
            let name = dto.get("name").and_then(Value::as_str).unwrap_or_default();
            state
                .services
                .themes
                .delete_theme(name)
                .await
                .map(|()| Value::Null)
                .map_err(|error| error.to_string())
        }
        "save_quick_reply_set" => {
            let payload = request.args.get("payload").cloned().unwrap_or(Value::Null);
            state
                .services
                .quick_replies
                .save_quick_reply_set(payload)
                .await
                .map(|()| Value::Null)
                .map_err(|error| error.to_string())
        }
        "delete_quick_reply_set" => {
            let payload = request.args.get("payload").cloned().unwrap_or(Value::Null);
            state
                .services
                .quick_replies
                .delete_quick_reply_set(payload)
                .await
                .map(|()| Value::Null)
                .map_err(|error| error.to_string())
        }
        "list_sprites" => {
            invoke_dto(&request.args, |dto: ListSpritesDto| async move {
                state
                    .services
                    .sprites
                    .list(dto)
                    .await
                    .map_err(ApplicationError::from)
            })
            .await
        }
        "upload_sprite" => match parse_dto::<UploadSpriteDto>(&request.args, "dto") {
            Ok(mut dto) => match resolve_staged_upload(&state, &dto.file_path).await {
                Ok(path) => {
                    dto.file_path = path.to_string_lossy().into_owned();
                    state
                        .services
                        .sprites
                        .upload(dto)
                        .await
                        .map(|()| Value::Null)
                        .map_err(|error| error.to_string())
                }
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        },
        "upload_sprite_pack" => match parse_dto::<UploadSpritePackDto>(&request.args, "dto") {
            Ok(mut dto) => match resolve_staged_upload(&state, &dto.file_path).await {
                Ok(path) => {
                    dto.file_path = path.to_string_lossy().into_owned();
                    state
                        .services
                        .sprites
                        .upload_pack(dto)
                        .await
                        .map(|count| json!(count))
                        .map_err(|error| error.to_string())
                }
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        },
        "delete_sprite" => {
            invoke_dto(&request.args, |dto: DeleteSpriteDto| async move {
                state
                    .services
                    .sprites
                    .delete(dto)
                    .await
                    .map(|()| Value::Null)
                    .map_err(ApplicationError::from)
            })
            .await
        }
        "search_chats" => match parse_args::<ChatSearchArgs>(&request.args) {
            Ok(args) => state
                .services
                .chats
                .search_chats(&args.query, args.character_filter.as_deref())
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string())),
            Err(error) => Err(error),
        },
        "list_group_chat_summaries" => match parse_args::<GroupChatSummaryArgs>(&request.args) {
            Ok(args) => state
                .services
                .group_chats
                .list_group_chat_summaries(
                    args.chat_ids.as_deref(),
                    args.include_metadata.unwrap_or(false),
                )
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string())),
            Err(error) => Err(error),
        },
        "list_recent_group_chat_summaries" => {
            match parse_args::<RecentGroupChatSummaryArgs>(&request.args) {
                Ok(args) => {
                    let pinned = args
                        .pinned
                        .unwrap_or_default()
                        .into_iter()
                        .map(Into::into)
                        .collect::<Vec<_>>();
                    state
                        .services
                        .group_chats
                        .list_recent_group_chat_summaries(
                            args.chat_ids.as_deref(),
                            args.include_metadata.unwrap_or(false),
                            args.max_entries.unwrap_or(usize::MAX),
                            &pinned,
                        )
                        .await
                        .map_err(|error| error.to_string())
                        .and_then(|value| {
                            serde_json::to_value(value).map_err(|error| error.to_string())
                        })
                }
                Err(error) => Err(error),
            }
        }
        "search_group_chats" => match parse_args::<GroupChatSearchArgs>(&request.args) {
            Ok(args) => state
                .services
                .group_chats
                .search_group_chats(&args.query, args.chat_ids.as_deref())
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string())),
            Err(error) => Err(error),
        },
        "search_character_chat_messages" => {
            match parse_args::<CharacterMessageQueryArgs>(&request.args) {
                Ok(args) => state
                    .services
                    .chats
                    .search_character_chat_messages(
                        &args.character_name,
                        &args.file_name,
                        args.query,
                    )
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|value| {
                        serde_json::to_value(value).map_err(|error| error.to_string())
                    }),
                Err(error) => Err(error),
            }
        }
        "search_group_chat_messages" => match parse_args::<GroupMessageQueryArgs>(&request.args) {
            Ok(args) => state
                .services
                .group_chats
                .search_group_chat_messages(&args.chat_id, args.query)
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string())),
            Err(error) => Err(error),
        },
        "find_last_character_chat_message" => {
            match parse_args::<CharacterFindLastArgs>(&request.args) {
                Ok(args) => state
                    .services
                    .chats
                    .find_last_character_chat_message(
                        &args.character_name,
                        &args.file_name,
                        args.query,
                    )
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|value| {
                        serde_json::to_value(value).map_err(|error| error.to_string())
                    }),
                Err(error) => Err(error),
            }
        }
        "find_last_group_chat_message" => match parse_args::<GroupFindLastArgs>(&request.args) {
            Ok(args) => state
                .services
                .group_chats
                .find_last_group_chat_message(&args.chat_id, args.query)
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string())),
            Err(error) => Err(error),
        },
        "get_chat_payload_tail" => match parse_args::<ChatPayloadTailArgs>(&request.args) {
            Ok(args) => match state
                .services
                .chats
                .get_chat_payload_tail_lines(&args.character_name, &args.file_name, args.max_lines)
                .await
            {
                Ok(value) => serde_json::to_value(value).map_err(|error| error.to_string()),
                Err(ApplicationError::NotFound(_)) if args.allow_not_found.unwrap_or(false) => {
                    Ok(json!({
                        "header": "", "lines": [], "cursor": {"offset": 0, "size": 0, "modifiedMillis": 0}, "hasMoreBefore": false
                    }))
                }
                Err(error) => Err(error.to_string()),
            },
            Err(error) => Err(error),
        },
        "get_chat_payload_before" => match parse_args::<ChatPayloadBeforeArgs>(&request.args) {
            Ok(args) => state
                .services
                .chats
                .get_chat_payload_before_lines(
                    &args.character_name,
                    &args.file_name,
                    args.cursor,
                    args.max_lines,
                )
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string())),
            Err(error) => Err(error),
        },
        "get_chat_payload_before_pages" => {
            match parse_args::<ChatPayloadBeforePagesArgs>(&request.args) {
                Ok(args) => state
                    .services
                    .chats
                    .get_chat_payload_before_pages_lines(
                        &args.character_name,
                        &args.file_name,
                        args.cursor,
                        args.max_lines,
                        args.max_pages,
                    )
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|value| {
                        serde_json::to_value(value).map_err(|error| error.to_string())
                    }),
                Err(error) => Err(error),
            }
        }
        "get_group_chat_payload_tail" => match parse_args::<GroupPayloadTailArgs>(&request.args) {
            Ok(args) => match state
                .services
                .group_chats
                .get_group_chat_payload_tail_lines(&args.id, args.max_lines)
                .await
            {
                Ok(value) => serde_json::to_value(value).map_err(|error| error.to_string()),
                Err(ApplicationError::NotFound(_)) if args.allow_not_found.unwrap_or(false) => {
                    Ok(json!({
                        "header": "", "lines": [], "cursor": {"offset": 0, "size": 0, "modifiedMillis": 0}, "hasMoreBefore": false
                    }))
                }
                Err(error) => Err(error.to_string()),
            },
            Err(error) => Err(error),
        },
        "get_group_chat_payload_before" => {
            match parse_args::<GroupPayloadBeforeArgs>(&request.args) {
                Ok(args) => state
                    .services
                    .group_chats
                    .get_group_chat_payload_before_lines(&args.id, args.cursor, args.max_lines)
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|value| {
                        serde_json::to_value(value).map_err(|error| error.to_string())
                    }),
                Err(error) => Err(error),
            }
        }
        "get_group_chat_payload_before_pages" => {
            match parse_args::<GroupPayloadBeforePagesArgs>(&request.args) {
                Ok(args) => state
                    .services
                    .group_chats
                    .get_group_chat_payload_before_pages_lines(
                        &args.id,
                        args.cursor,
                        args.max_lines,
                        args.max_pages,
                    )
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|value| {
                        serde_json::to_value(value).map_err(|error| error.to_string())
                    }),
                Err(error) => Err(error),
            }
        }
        "get_character_chat_store_json" => {
            let character = string_arg(&request.args, "character_name", "characterName");
            let file = string_arg(&request.args, "file_name", "fileName");
            let namespace = string_arg(&request.args, "namespace", "namespace");
            let key = string_arg(&request.args, "key", "key");
            state
                .services
                .chats
                .get_character_chat_store_json(character, file, namespace, key)
                .await
                .map_err(|error| error.to_string())
        }
        "set_character_chat_store_json" | "update_character_chat_store_json" => {
            let character = string_arg(&request.args, "character_name", "characterName");
            let file = string_arg(&request.args, "file_name", "fileName");
            let namespace = string_arg(&request.args, "namespace", "namespace");
            let key = string_arg(&request.args, "key", "key");
            match request.args.get("value").cloned() {
                Some(value) if request.command == "set_character_chat_store_json" => state
                    .services
                    .chats
                    .set_character_chat_store_json(character, file, namespace, key, value)
                    .await
                    .map(|()| Value::Null)
                    .map_err(|error| error.to_string()),
                Some(value) => state
                    .services
                    .chats
                    .update_character_chat_store_json(character, file, namespace, key, value)
                    .await
                    .map(|()| Value::Null)
                    .map_err(|error| error.to_string()),
                None => Err("Missing required field: value".to_string()),
            }
        }
        "rename_character_chat_store_key" => {
            let character = string_arg(&request.args, "character_name", "characterName");
            let file = string_arg(&request.args, "file_name", "fileName");
            let namespace = string_arg(&request.args, "namespace", "namespace");
            let key = string_arg(&request.args, "key", "key");
            let new_key = string_arg(&request.args, "new_key", "newKey");
            state
                .services
                .chats
                .rename_character_chat_store_key(character, file, namespace, key, new_key)
                .await
                .map(|()| Value::Null)
                .map_err(|error| error.to_string())
        }
        "delete_character_chat_store_json" => {
            let character = string_arg(&request.args, "character_name", "characterName");
            let file = string_arg(&request.args, "file_name", "fileName");
            let namespace = string_arg(&request.args, "namespace", "namespace");
            let key = string_arg(&request.args, "key", "key");
            state
                .services
                .chats
                .delete_character_chat_store_json(character, file, namespace, key)
                .await
                .map(|()| Value::Null)
                .map_err(|error| error.to_string())
        }
        "list_character_chat_store_keys" => {
            let character = string_arg(&request.args, "character_name", "characterName");
            let file = string_arg(&request.args, "file_name", "fileName");
            let namespace = string_arg(&request.args, "namespace", "namespace");
            state
                .services
                .chats
                .list_character_chat_store_keys(character, file, namespace)
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string()))
        }
        "get_group_chat_store_json" => {
            let id = string_arg(&request.args, "chat_id", "chatId");
            let namespace = string_arg(&request.args, "namespace", "namespace");
            let key = string_arg(&request.args, "key", "key");
            state
                .services
                .group_chats
                .get_group_chat_store_json(id, namespace, key)
                .await
                .map_err(|error| error.to_string())
        }
        "set_group_chat_store_json" | "update_group_chat_store_json" => {
            let id = string_arg(&request.args, "chat_id", "chatId");
            let namespace = string_arg(&request.args, "namespace", "namespace");
            let key = string_arg(&request.args, "key", "key");
            match request.args.get("value").cloned() {
                Some(value) if request.command == "set_group_chat_store_json" => state
                    .services
                    .group_chats
                    .set_group_chat_store_json(id, namespace, key, value)
                    .await
                    .map(|()| Value::Null)
                    .map_err(|error| error.to_string()),
                Some(value) => state
                    .services
                    .group_chats
                    .update_group_chat_store_json(id, namespace, key, value)
                    .await
                    .map(|()| Value::Null)
                    .map_err(|error| error.to_string()),
                None => Err("Missing required field: value".to_string()),
            }
        }
        "rename_group_chat_store_key" => {
            let id = string_arg(&request.args, "chat_id", "chatId");
            let namespace = string_arg(&request.args, "namespace", "namespace");
            let key = string_arg(&request.args, "key", "key");
            let new_key = string_arg(&request.args, "new_key", "newKey");
            state
                .services
                .group_chats
                .rename_group_chat_store_key(id, namespace, key, new_key)
                .await
                .map(|()| Value::Null)
                .map_err(|error| error.to_string())
        }
        "delete_group_chat_store_json" => {
            let id = string_arg(&request.args, "chat_id", "chatId");
            let namespace = string_arg(&request.args, "namespace", "namespace");
            let key = string_arg(&request.args, "key", "key");
            state
                .services
                .group_chats
                .delete_group_chat_store_json(id, namespace, key)
                .await
                .map(|()| Value::Null)
                .map_err(|error| error.to_string())
        }
        "list_group_chat_store_keys" => {
            let id = string_arg(&request.args, "chat_id", "chatId");
            let namespace = string_arg(&request.args, "namespace", "namespace");
            state
                .services
                .group_chats
                .list_group_chat_store_keys(id, namespace)
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string()))
        }
        "delete_group_chat" => {
            invoke_dto(&request.args, |dto: DeleteGroupChatDto| async move {
                state
                    .services
                    .group_chats
                    .delete_group_chat(dto)
                    .await
                    .map(|()| Value::Null)
            })
            .await
        }
        "rename_group_chat" => {
            invoke_dto(&request.args, |dto: RenameGroupChatDto| async move {
                state
                    .services
                    .group_chats
                    .rename_group_chat(dto)
                    .await
                    .map(|value| json!(value))
            })
            .await
        }
        "restore_group_chat_backup" => {
            invoke_dto(&request.args, |dto: RestoreGroupChatBackupDto| async move {
                state
                    .services
                    .group_chats
                    .restore_group_chat_backup(dto)
                    .await
                    .map(|value| json!(value))
            })
            .await
        }
        "list_recent_chat_summaries" => match parse_args::<RecentChatSummaryArgs>(&request.args) {
            Ok(args) => {
                let pinned = args
                    .pinned
                    .unwrap_or_default()
                    .into_iter()
                    .map(Into::into)
                    .collect::<Vec<_>>();
                state
                    .services
                    .chats
                    .list_recent_chat_summaries(
                        args.character_filter.as_deref(),
                        args.include_metadata.unwrap_or(false),
                        args.max_entries.unwrap_or(usize::MAX),
                        &pinned,
                    )
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|value| {
                        serde_json::to_value(value).map_err(|error| error.to_string())
                    })
            }
            Err(error) => Err(error),
        },
        "list_chat_backups" => state
            .services
            .chats
            .list_chat_backups()
            .await
            .map_err(|error| error.to_string())
            .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string())),
        "backup_chat" => match parse_args::<ChatBackupArgs>(&request.args) {
            Ok(args) => state
                .services
                .chats
                .backup_chat(&args.character_name, &args.file_name)
                .await
                .map(|()| Value::Null)
                .map_err(|error| error.to_string()),
            Err(error) => Err(error),
        },
        "delete_chat_backup" => match parse_args::<ChatBackupNameArgs>(&request.args) {
            Ok(args) => state
                .services
                .chats
                .delete_chat_backup(&args.name)
                .await
                .map(|()| Value::Null)
                .map_err(|error| error.to_string()),
            Err(error) => Err(error),
        },
        "restore_character_chat_backup" => {
            match parse_dto::<RestoreCharacterChatBackupDto>(&request.args, "dto") {
                Ok(dto) => state
                    .services
                    .chats
                    .restore_character_chat_backup(dto)
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|value| {
                        serde_json::to_value(value).map_err(|error| error.to_string())
                    }),
                Err(error) => Err(error),
            }
        }
        "list_chat_backup_catalog" => state
            .services
            .chats
            .list_chat_backup_catalog()
            .await
            .map_err(|error| error.to_string())
            .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string())),
        "open_chat_backup_download" => {
            let name = string_arg(&request.args, "name", "name");
            match state.services.chats.open_chat_backup_download(name).await {
                Ok(reader) => {
                    let rid = state.next_chat_byte_reader.fetch_add(1, Ordering::Relaxed);
                    state
                        .chat_byte_readers
                        .lock()
                        .await
                        .insert(rid, WebChatByteReader { reader });
                    Ok(json!(rid))
                }
                Err(error) => Err(error.to_string()),
            }
        }
        "read_chat_bytes" => {
            let rid = request
                .args
                .get("rid")
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok());
            match rid {
                Some(rid) => {
                    let mut readers = state.chat_byte_readers.lock().await;
                    match readers.get_mut(&rid) {
                        Some(reader) => {
                            const READ_CHUNK_BYTES: usize = 512 * 1024;
                            let mut chunk = vec![0; READ_CHUNK_BYTES];
                            match reader.reader.read(&mut chunk).await {
                                Ok(bytes_read) => {
                                    chunk.truncate(bytes_read);
                                    Ok(json!(chunk))
                                }
                                Err(error) => Err(error.to_string()),
                            }
                        }
                        None => Err("Chat backup reader not found".to_string()),
                    }
                }
                None => Err("Invalid chat backup reader id".to_string()),
            }
        }
        "plugin:resources|close" => {
            let rid = request
                .args
                .get("rid")
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok());
            if let Some(rid) = rid {
                state.chat_byte_readers.lock().await.remove(&rid);
                state.cold_chat_sources.lock().await.remove(&rid);
            }
            Ok(Value::Null)
        }
        "open_cold_chat" => {
            let target = request.args.get("target").cloned().unwrap_or(Value::Null);
            let allow_not_found = request
                .args
                .get("allowNotFound")
                .or_else(|| request.args.get("allow_not_found"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            match serde_json::from_value::<ChatHistoryLocator>(target) {
                Ok(target) => match state
                    .services
                    .chat_commits
                    .open_swipe_source(target, allow_not_found)
                    .await
                {
                    Ok(Some(source)) => {
                        let source_id = state.next_chat_byte_reader.fetch_add(1, Ordering::Relaxed);
                        let reader_id = state.next_chat_byte_reader.fetch_add(1, Ordering::Relaxed);
                        let reader = source.clone().projection(source_id);
                        state
                            .cold_chat_sources
                            .lock()
                            .await
                            .insert(source_id, source);
                        state
                            .chat_byte_readers
                            .lock()
                            .await
                            .insert(reader_id, WebChatByteReader { reader });
                        serde_json::to_value(OpenColdChatResult {
                            source_id,
                            reader_id,
                        })
                        .map_err(|error| error.to_string())
                    }
                    Ok(None) => Ok(Value::Null),
                    Err(error) => Err(error.to_string()),
                },
                Err(error) => Err(format!("Invalid cold chat target: {error}")),
            }
        }
        "open_cold_swipe_record" => {
            let source_id = request
                .args
                .get("sourceId")
                .or_else(|| request.args.get("source_id"))
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok());
            let record = request
                .args
                .get("record")
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok());
            match (source_id, record) {
                (Some(source_id), Some(record)) => {
                    let source = state
                        .cold_chat_sources
                        .lock()
                        .await
                        .get(&source_id)
                        .cloned();
                    match source {
                        Some(source) => match source.record(record).await {
                            Ok(reader) => {
                                let rid =
                                    state.next_chat_byte_reader.fetch_add(1, Ordering::Relaxed);
                                state
                                    .chat_byte_readers
                                    .lock()
                                    .await
                                    .insert(rid, WebChatByteReader { reader });
                                Ok(json!(rid))
                            }
                            Err(error) => Err(error.to_string()),
                        },
                        None => Err("Cold chat source expired; reopen the chat".to_string()),
                    }
                }
                _ => Err("Invalid cold swipe record reference".to_string()),
            }
        }
        "import_character_chats" => {
            match parse_dto::<ImportCharacterChatsDto>(&request.args, "dto") {
                Ok(mut dto) => match resolve_staged_upload(&state, &dto.file_path).await {
                    Ok(path) => {
                        dto.file_path = path.to_string_lossy().into_owned();
                        state
                            .services
                            .chats
                            .import_character_chats(dto)
                            .await
                            .map_err(|error| error.to_string())
                            .and_then(|value| {
                                serde_json::to_value(value).map_err(|error| error.to_string())
                            })
                    }
                    Err(error) => Err(error),
                },
                Err(error) => Err(error),
            }
        }
        "import_group_chat_payload" => {
            match parse_dto::<ImportGroupChatDto>(&request.args, "dto") {
                Ok(mut dto) => match resolve_staged_upload(&state, &dto.file_path).await {
                    Ok(path) => {
                        dto.file_path = path.to_string_lossy().into_owned();
                        state
                            .services
                            .group_chats
                            .import_group_chat(dto)
                            .await
                            .map(Value::String)
                            .map_err(|error| error.to_string())
                    }
                    Err(error) => Err(error),
                },
                Err(error) => Err(error),
            }
        }
        "get_chat_backup_storage_stats" => state
            .services
            .settings
            .get_chat_backup_storage_stats()
            .await
            .map_err(|error| error.to_string())
            .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string())),
        "create_settings_snapshot" => state
            .services
            .settings
            .create_snapshot()
            .await
            .map(|()| Value::Null)
            .map_err(|error| error.to_string()),
        "get_settings_snapshots" => state
            .services
            .settings
            .get_snapshots()
            .await
            .map_err(|error| error.to_string())
            .and_then(|value: Vec<SettingsSnapshotDto>| {
                serde_json::to_value(value).map_err(|error| error.to_string())
            }),
        "load_settings_snapshot" => match parse_args::<SnapshotNameArgs>(&request.args) {
            Ok(args) => state
                .services
                .settings
                .load_snapshot(&args.name)
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string())),
            Err(error) => Err(error),
        },
        "restore_settings_snapshot" => match parse_args::<SnapshotNameArgs>(&request.args) {
            Ok(args) => state
                .services
                .settings
                .restore_snapshot(&args.name)
                .await
                .map(|()| Value::Null)
                .map_err(|error| error.to_string()),
            Err(error) => Err(error),
        },
        "list_mcp_servers" => state
            .services
            .mcp
            .list_servers()
            .await
            .map_err(|error| error.to_string())
            .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string())),
        "create_mcp_server" => {
            invoke_dto(&request.args, |dto: CreateMcpServerDto| async move {
                state
                    .services
                    .mcp
                    .create_server(
                        dto.display_name,
                        dto.endpoint,
                        dto.headers,
                        dto.protocol_version,
                    )
                    .await
            })
            .await
        }
        "update_mcp_server" => {
            invoke_dto(&request.args, |dto: UpdateMcpServerDto| async move {
                state
                    .services
                    .mcp
                    .update_server(
                        &dto.registration_id,
                        dto.display_name,
                        dto.endpoint,
                        dto.headers,
                        dto.protocol_version,
                    )
                    .await
            })
            .await
        }
        "set_mcp_server_state" => {
            invoke_dto(&request.args, |dto: SetMcpServerStateDto| async move {
                state
                    .services
                    .mcp
                    .set_server_state(&dto.registration_id, dto.state)
                    .await
            })
            .await
        }
        "remove_mcp_server" => {
            invoke_dto(&request.args, |dto: McpRegistrationIdDto| async move {
                state
                    .services
                    .mcp
                    .remove_server(&dto.registration_id)
                    .await
                    .map(|()| Value::Null)
            })
            .await
        }
        "discover_mcp_tools" => {
            invoke_dto(&request.args, |dto: McpRegistrationIdDto| async move {
                state
                    .services
                    .mcp
                    .discover_tools(&dto.registration_id)
                    .await
            })
            .await
        }
        "refresh_mcp_tools" => {
            invoke_dto(&request.args, |dto: McpRegistrationIdDto| async move {
                state.services.mcp.refresh_tools(&dto.registration_id).await
            })
            .await
        }
        "set_mcp_tool_permission" => {
            invoke_dto(&request.args, |dto: SetMcpToolPermissionDto| async move {
                state
                    .services
                    .mcp
                    .set_tool_permission(&dto.registration_id, dto.native_name, dto.permission)
                    .await
            })
            .await
        }
        "set_mcp_tool_description_override" => {
            invoke_dto(
                &request.args,
                |dto: SetMcpToolDescriptionOverrideDto| async move {
                    state
                        .services
                        .mcp
                        .set_tool_description_override(
                            &dto.registration_id,
                            dto.native_name,
                            dto.override_,
                        )
                        .await
                },
            )
            .await
        }
        "start_mcp_test_call" => {
            invoke_dto(&request.args, |dto: McpTestCallIdDto| async move {
                state
                    .services
                    .mcp
                    .start_call(&dto.call_id)
                    .await
                    .map(|()| Value::Null)
            })
            .await
        }
        "test_mcp_tool_call" => {
            invoke_dto(&request.args, |dto: TestMcpToolCallDto| async move {
                state
                    .services
                    .mcp
                    .test_call(
                        &dto.call_id,
                        &dto.registration_id,
                        dto.native_name,
                        dto.arguments_json,
                    )
                    .await
            })
            .await
        }
        "cancel_mcp_test_call" => {
            invoke_dto(&request.args, |dto: McpTestCallIdDto| async move {
                state
                    .services
                    .mcp
                    .cancel_call(&dto.call_id)
                    .await
                    .map(|()| Value::Null)
            })
            .await
        }
        "list_legacy_mcp_tools" => state
            .services
            .mcp
            .list_legacy_tools_cached()
            .await
            .map_err(|error| error.to_string())
            .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string())),
        "start_legacy_mcp_tool_call" => {
            invoke_dto(&request.args, |dto: McpExecutionCallIdDto| async move {
                state
                    .services
                    .mcp
                    .start_call(&dto.execution_call_id)
                    .await
                    .map(|()| Value::Null)
            })
            .await
        }
        "call_legacy_mcp_tool" => {
            invoke_dto(&request.args, |dto: CallLegacyMcpToolDto| async move {
                state
                    .services
                    .mcp
                    .call_legacy_tool(&dto.execution_call_id, &dto.tool_id, dto.arguments_json)
                    .await
            })
            .await
        }
        "cancel_legacy_mcp_tool_call" => {
            invoke_dto(&request.args, |dto: McpExecutionCallIdDto| async move {
                state
                    .services
                    .mcp
                    .cancel_call(&dto.execution_call_id)
                    .await
                    .map(|()| Value::Null)
            })
            .await
        }
        "list_skills" => {
            let scope = request
                .args
                .get("scope")
                .cloned()
                .or_else(|| request.args.get("filter").cloned())
                .map(serde_json::from_value)
                .transpose()
                .map_err(|error| error.to_string())
                .map(|scope| scope.unwrap_or_default());
            match scope {
                Ok(scope) => state
                    .services
                    .skills
                    .list_skills(scope)
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|value| {
                        serde_json::to_value(value).map_err(|error| error.to_string())
                    }),
                Err(error) => Err(error),
            }
        }
        "list_skill_files" => match parse_args::<SkillFilesArgs>(&request.args) {
            Ok(args) => state
                .services
                .skills
                .list_skill_files(args.scope.unwrap_or_default(), &args.name)
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string())),
            Err(error) => Err(error),
        },
        "discover_skill_imports" => match parse_args::<SkillImportArgs>(&request.args) {
            Ok(args) => state
                .services
                .skills
                .discover_imports(args.input)
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string())),
            Err(error) => Err(error),
        },
        "discard_skill_import_archive" => match parse_args::<SkillDiscardArgs>(&request.args) {
            Ok(args) => state
                .services
                .skills
                .discard_import_archive(&args.path)
                .await
                .map(|()| Value::Null)
                .map_err(|error| error.to_string()),
            Err(error) => Err(error),
        },
        "preview_skill_import" => match parse_args::<SkillPreviewArgs>(&request.args) {
            Ok(args) => state
                .services
                .skills
                .preview_import(args.input, args.target_scope.unwrap_or_default())
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string())),
            Err(error) => Err(error),
        },
        "install_skill_import" => match parse_args::<SkillInstallArgs>(&request.args) {
            Ok(args) => state
                .services
                .skills
                .install_import(args.request)
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string())),
            Err(error) => Err(error),
        },
        "read_skill_file" => match parse_args::<SkillReadArgs>(&request.args) {
            Ok(args) => state
                .services
                .skills
                .read_skill_file(SkillReadRequest {
                    frozen_macros: None,
                    scope: args.scope.unwrap_or_default(),
                    name: args.name,
                    path: args.path,
                    start_line: args.start_line,
                    line_count: args.line_count,
                    max_output_chars: DEFAULT_SKILL_READ_FALLBACK_MAX_CHARS,
                })
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string())),
            Err(error) => Err(error),
        },
        "write_skill_file" => match parse_args::<SkillWriteArgs>(&request.args) {
            Ok(args) => state
                .services
                .skills
                .write_skill_file(SkillWriteRequest {
                    scope: args.scope.unwrap_or_default(),
                    name: args.name,
                    path: args.path,
                    content: args.content,
                    expected_sha256: args.expected_sha256,
                })
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string())),
            Err(error) => Err(error),
        },
        "export_skill" => match parse_args::<SkillNameScopeArgs>(&request.args) {
            Ok(args) => match state
                .services
                .skills
                .export_skill(args.scope.unwrap_or_default(), &args.name)
                .await
            {
                Ok(exported) => Ok(
                    json!({ "fileName": exported.file_name, "contentBase64": BASE64_STANDARD.encode(exported.bytes), "sha256": exported.sha256 }),
                ),
                Err(error) => Err(error.to_string()),
            },
            Err(error) => Err(error),
        },
        "delete_skill" => match parse_args::<SkillNameScopeArgs>(&request.args) {
            Ok(args) => state
                .services
                .skills
                .delete_skill(args.scope.unwrap_or_default(), &args.name)
                .await
                .map(|()| Value::Null)
                .map_err(|error| error.to_string()),
            Err(error) => Err(error),
        },
        "move_skill" => match parse_args::<SkillMoveArgs>(&request.args) {
            Ok(args) => state
                .services
                .skills
                .move_skill(args.request)
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string())),
            Err(error) => Err(error),
        },
        "retarget_skill_scope" => match parse_args::<SkillRetargetArgs>(&request.args) {
            Ok(args) => state
                .services
                .skills
                .retarget_scope(args.request)
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string())),
            Err(error) => Err(error),
        },
        "download_skill_import_url" => match parse_args::<SkillDownloadArgs>(&request.args) {
            Ok(args) => state
                .services
                .skills
                .download_import_url(&args.url)
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string())),
            Err(error) => Err(error),
        },
        "get_extensions" => state
            .services
            .extensions
            .get_extensions()
            .await
            .map_err(|error| error.to_string())
            .and_then(|extensions| {
                serde_json::to_value(extensions).map_err(|error| error.to_string())
            }),
        "install_extension" => {
            let url = string_arg(&request.args, "url", "url");
            let global = request
                .args
                .get("global")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let branch = request
                .args
                .get("branch")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|branch| !branch.is_empty())
                .map(str::to_string);
            state
                .services
                .extensions
                .install_extension(url, global, branch)
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| serde_json::to_value(result).map_err(|error| error.to_string()))
        }
        "update_extension" => {
            let name = string_arg(&request.args, "extension_name", "extensionName");
            let global = request
                .args
                .get("global")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            state
                .services
                .extensions
                .update_extension(name, global)
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| serde_json::to_value(result).map_err(|error| error.to_string()))
        }
        "delete_extension" => {
            let name = string_arg(&request.args, "extension_name", "extensionName");
            let global = request
                .args
                .get("global")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            state
                .services
                .extensions
                .delete_extension(name, global)
                .await
                .map(|()| Value::Null)
                .map_err(|error| error.to_string())
        }
        "get_extension_version" => {
            let name = string_arg(&request.args, "extension_name", "extensionName");
            let global = request
                .args
                .get("global")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            state
                .services
                .extensions
                .get_extension_version(name, global)
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| serde_json::to_value(result).map_err(|error| error.to_string()))
        }
        "get_extension_branches" => {
            let name = string_arg(&request.args, "extension_name", "extensionName");
            let global = request
                .args
                .get("global")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            state
                .services
                .extensions
                .get_extension_branches(name, global)
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| serde_json::to_value(result).map_err(|error| error.to_string()))
        }
        "switch_extension_branch" => {
            let name = string_arg(&request.args, "extension_name", "extensionName");
            let branch = string_arg(&request.args, "branch", "branch");
            let global = request
                .args
                .get("global")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            state
                .services
                .extensions
                .switch_extension_branch(name, branch, global)
                .await
                .map(|()| Value::Null)
                .map_err(|error| error.to_string())
        }
        "move_extension" => {
            let name = string_arg(&request.args, "extension_name", "extensionName");
            let source = string_arg(&request.args, "source", "source");
            let destination = string_arg(&request.args, "destination", "destination");
            state
                .services
                .extensions
                .move_extension(name, source, destination)
                .await
                .map(|()| Value::Null)
                .map_err(|error| error.to_string())
        }
        "get_extension_store_json" => {
            let namespace = string_arg(&request.args, "namespace", "namespace");
            let key = string_arg(&request.args, "key", "key");
            let table = request.args.get("table").and_then(Value::as_str);
            state
                .services
                .extension_store
                .get_json(namespace, table, key)
                .await
                .map_err(|error| error.to_string())
        }
        "try_get_extension_store_json" => {
            let namespace = string_arg(&request.args, "namespace", "namespace");
            let key = string_arg(&request.args, "key", "key");
            let table = request.args.get("table").and_then(Value::as_str);
            state
                .services
                .extension_store
                .try_get_json(namespace, table, key)
                .await
                .map(|value| match value {
                    Some(value) => json!({ "found": true, "value": value }),
                    None => json!({ "found": false }),
                })
                .map_err(|error| error.to_string())
        }
        "set_extension_store_json" | "update_extension_store_json" => {
            let namespace = string_arg(&request.args, "namespace", "namespace");
            let key = string_arg(&request.args, "key", "key");
            let table = request.args.get("table").and_then(Value::as_str);
            if let Some(value) = request.args.get("value").cloned() {
                let result = if request.command == "set_extension_store_json" {
                    state
                        .services
                        .extension_store
                        .set_json(namespace, table, key, value)
                        .await
                } else {
                    state
                        .services
                        .extension_store
                        .update_json(namespace, table, key, value)
                        .await
                };
                result
                    .map(|()| Value::Null)
                    .map_err(|error| error.to_string())
            } else {
                Err("Missing required field: value".to_owned())
            }
        }
        "rename_extension_store_key" => {
            let namespace = string_arg(&request.args, "namespace", "namespace");
            let key = string_arg(&request.args, "key", "key");
            let new_key = string_arg(&request.args, "new_key", "newKey");
            let table = request.args.get("table").and_then(Value::as_str);
            state
                .services
                .extension_store
                .rename_json_key(namespace, table, key, new_key)
                .await
                .map(|()| Value::Null)
                .map_err(|error| error.to_string())
        }
        "delete_extension_store_json" => {
            let namespace = string_arg(&request.args, "namespace", "namespace");
            let key = string_arg(&request.args, "key", "key");
            let table = request.args.get("table").and_then(Value::as_str);
            state
                .services
                .extension_store
                .delete_json(namespace, table, key)
                .await
                .map(|()| Value::Null)
                .map_err(|error| error.to_string())
        }
        "list_extension_store_keys" => {
            let namespace = string_arg(&request.args, "namespace", "namespace");
            let table = request.args.get("table").and_then(Value::as_str);
            state
                .services
                .extension_store
                .list_json_keys(namespace, table)
                .await
                .map(|keys| json!(keys))
                .map_err(|error| error.to_string())
        }
        "list_extension_store_tables" => {
            let namespace = string_arg(&request.args, "namespace", "namespace");
            state
                .services
                .extension_store
                .list_tables(namespace)
                .await
                .map(|tables| json!(tables))
                .map_err(|error| error.to_string())
        }
        "delete_extension_store_table" => {
            let namespace = string_arg(&request.args, "namespace", "namespace");
            let table = string_arg(&request.args, "table", "table");
            state
                .services
                .extension_store
                .delete_table(namespace, table)
                .await
                .map(|()| Value::Null)
                .map_err(|error| error.to_string())
        }
        "get_extension_store_blob" => {
            let namespace = string_arg(&request.args, "namespace", "namespace");
            let key = string_arg(&request.args, "key", "key");
            let table = request.args.get("table").and_then(Value::as_str);
            state
                .services
                .extension_store
                .get_blob(namespace, table, key)
                .await
                .map(|bytes| {
                    json!({
                        "content_base64": BASE64_STANDARD.encode(bytes),
                        "mime_type": mime_guess::from_path(key).first_or_octet_stream().essence_str(),
                    })
                })
                .map_err(|error| error.to_string())
        }
        "set_extension_store_blob" => {
            let namespace = string_arg(&request.args, "namespace", "namespace");
            let key = string_arg(&request.args, "key", "key");
            let table = request.args.get("table").and_then(Value::as_str);
            let encoded = string_arg(&request.args, "data_base64", "dataBase64");
            match BASE64_STANDARD.decode(encoded) {
                Ok(bytes) => state
                    .services
                    .extension_store
                    .set_blob(namespace, table, key, bytes)
                    .await
                    .map(|()| Value::Null)
                    .map_err(|error| error.to_string()),
                Err(error) => Err(error.to_string()),
            }
        }
        "delete_extension_store_blob" => {
            let namespace = string_arg(&request.args, "namespace", "namespace");
            let key = string_arg(&request.args, "key", "key");
            let table = request.args.get("table").and_then(Value::as_str);
            state
                .services
                .extension_store
                .delete_blob(namespace, table, key)
                .await
                .map(|()| Value::Null)
                .map_err(|error| error.to_string())
        }
        "list_extension_store_blob_keys" => {
            let namespace = string_arg(&request.args, "namespace", "namespace");
            let table = request.args.get("table").and_then(Value::as_str);
            state
                .services
                .extension_store
                .list_blob_keys(namespace, table)
                .await
                .map(|keys| json!(keys))
                .map_err(|error| error.to_string())
        }
        "wait_for_backend_ready" => Ok(Value::Null),
        "backend_error_bridge_ready" => Ok(json!(
            state.dev_logs.mark_backend_error_bridge_ready_and_drain()
        )),
        "list_agent_profiles" => state
            .services
            .agent_profiles
            .list_profiles()
            .await
            .and_then(|profiles| {
                serde_json::to_value(AgentListProfilesResultDto {
                    profiles: profiles.profiles,
                    issues: profiles.issues,
                })
                .map_err(|error| ApplicationError::InternalError(error.to_string()))
            })
            .map_err(|error| error.to_string()),
        "list_agent_tools" => state
            .services
            .agent_runtime
            .tool_catalog_items()
            .await
            .map_err(|error| error.to_string())
            .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string())),
        "load_agent_profile" => {
            invoke_dto(&request.args, |dto: AgentProfileIdDto| async move {
                state
                    .services
                    .agent_profiles
                    .load_profile(&dto.profile_id)
                    .await
                    .map(|profile| AgentLoadProfileResultDto { profile })
            })
            .await
        }
        "diagnose_agent_profile" => {
            invoke_dto(&request.args, |dto: AgentProfileIdDto| async move {
                state
                    .services
                    .agent_profile_diagnostics
                    .diagnose_profile(&dto.profile_id, state.services.agent_runtime.tool_catalog())
                    .await
            })
            .await
        }
        "save_agent_profile" => {
            invoke_dto(&request.args, |dto: AgentSaveProfileDto| async move {
                state
                    .services
                    .agent_profiles
                    .save_profile(dto.profile, state.services.agent_runtime.tool_catalog())
                    .await
                    .map(|()| Value::Null)
            })
            .await
        }
        "delete_agent_profile" => {
            invoke_dto(&request.args, |dto: AgentProfileIdDto| async move {
                state
                    .services
                    .agent_profiles
                    .delete_profile(&dto.profile_id)
                    .await
                    .map(|()| Value::Null)
            })
            .await
        }
        "repair_agent_profile_file" => {
            invoke_dto(&request.args, |dto: AgentRepairProfileFileDto| async move {
                state
                    .services
                    .agent_profiles
                    .repair_profile_file(&dto.profile_id, dto.action)
                    .await
                    .map(|()| Value::Null)
            })
            .await
        }
        "retarget_agent_profile_preset_refs" => {
            invoke_dto(
                &request.args,
                |dto: AgentRetargetPresetRefsDto| async move {
                    state
                        .services
                        .agent_profiles
                        .retarget_preset_refs(dto.from, dto.to)
                        .await
                        .map(|result| {
                            let updated = result.profile_ids.len();
                            AgentRetargetPresetRefsResultDto {
                                updated,
                                profile_ids: result
                                    .profile_ids
                                    .into_iter()
                                    .map(|id| id.as_str().to_string())
                                    .collect(),
                            }
                        })
                },
            )
            .await
        }
        "resolve_agent_system_prompt" => {
            invoke_dto(
                &request.args,
                |dto: AgentResolveSystemPromptDto| async move {
                    state
                        .services
                        .agent_runtime
                        .resolve_agent_system_prompt(dto.profile_id.as_deref())
                        .await
                        .map(|agent_system_prompt| AgentResolveSystemPromptResultDto {
                            agent_system_prompt,
                        })
                },
            )
            .await
        }
        "prepare_agent_prompt_assembly" => {
            invoke_dto(
                &request.args,
                |dto: AgentPreparePromptAssemblyDto| async move {
                    let profile = state
                        .services
                        .prompt_assembly
                        .resolve_profile(
                            dto.profile_id.as_deref(),
                            state.services.agent_runtime.tool_catalog(),
                        )
                        .await?;
                    let tools = state
                        .services
                        .agent_runtime
                        .visible_model_tools(&profile)
                        .await?;
                    state
                        .services
                        .prompt_assembly
                        .prepare_frontend_prompt_assembly(dto, profile, &tools)
                        .await
                },
            )
            .await
        }
        "build_agent_current_model_connection_snapshot" => {
            invoke_dto(
                &request.args,
                |dto: AgentBuildCurrentModelConnectionSnapshotDto| async move {
                    state
                        .services
                        .prompt_assembly
                        .build_current_model_connection_snapshot(
                            &dto.settings,
                            &dto.model,
                            dto.secret_id.as_deref(),
                        )
                        .map(|current_model_connection| {
                            AgentBuildCurrentModelConnectionSnapshotResultDto {
                                current_model_connection,
                            }
                        })
                },
            )
            .await
        }
        "apply_agent_current_model_connection_snapshot" => {
            invoke_dto(
                &request.args,
                |dto: AgentApplyCurrentModelConnectionSnapshotDto| async move {
                    state
                        .services
                        .prompt_assembly
                        .apply_current_model_connection_snapshot(
                            dto.settings,
                            &dto.current_model_connection,
                        )
                        .map(
                            |settings| AgentApplyCurrentModelConnectionSnapshotResultDto {
                                settings,
                            },
                        )
                },
            )
            .await
        }
        "start_agent_run" => {
            invoke_dto(&request.args, |dto: AgentStartRunDto| async move {
                state.services.agent_runtime.start_run(dto).await
            })
            .await
        }
        "cancel_agent_run" => {
            invoke_dto(&request.args, |dto: AgentCancelRunDto| async move {
                state.services.agent_runtime.cancel_run(dto).await
            })
            .await
        }
        "submit_agent_run_guidance" => {
            invoke_dto(&request.args, |dto: AgentSubmitGuidanceDto| async move {
                state.services.agent_runtime.submit_guidance(dto).await
            })
            .await
        }
        "read_agent_run_events" => {
            invoke_dto(&request.args, |dto: AgentReadEventsDto| async move {
                state.services.agent_runtime.read_events(dto).await
            })
            .await
        }
        "read_agent_workspace_file" => {
            invoke_dto(&request.args, |dto: AgentReadWorkspaceFileDto| async move {
                state.services.agent_runtime.read_workspace_file(dto).await
            })
            .await
        }
        "read_agent_task_detail" => {
            invoke_dto(&request.args, |dto: AgentReadTaskDetailDto| async move {
                state.services.agent_runtime.read_task_detail(dto).await
            })
            .await
        }
        "read_agent_model_turn" => {
            invoke_dto(&request.args, |dto: AgentReadModelTurnDto| async move {
                state.services.agent_runtime.read_model_turn(dto).await
            })
            .await
        }
        "read_agent_run_checkpoint" => {
            invoke_dto(&request.args, |dto: AgentReadRunCheckpointDto| async move {
                state.services.agent_runtime.read_run_checkpoint(dto).await
            })
            .await
        }
        "finish_agent_run_presentation" => {
            invoke_dto(
                &request.args,
                |dto: AgentFinishRunPresentationDto| async move {
                    state
                        .services
                        .agent_runtime
                        .finish_run_presentation(dto)
                        .await
                        .map(|()| Value::Null)
                },
            )
            .await
        }
        "resume_agent_run" => {
            invoke_dto(&request.args, |dto: AgentResumeRunDto| async move {
                state.services.agent_runtime.resume_run(dto).await
            })
            .await
        }
        "read_agent_prompt_assembly_request" => {
            invoke_dto(
                &request.args,
                |dto: AgentReadPromptAssemblyRequestDto| async {
                    state
                        .services
                        .agent_runtime
                        .read_prompt_assembly_request(dto)
                        .await
                },
            )
            .await
        }
        "resolve_agent_chat_commit" => {
            invoke_dto(&request.args, |dto: AgentResolveChatCommitDto| async move {
                state
                    .services
                    .agent_runtime
                    .resolve_chat_commit(dto)
                    .await
                    .map(|()| Value::Null)
            })
            .await
        }
        "resolve_agent_prompt_assembly" => {
            invoke_dto(
                &request.args,
                |dto: AgentResolvePromptAssemblyDto| async move {
                    state
                        .services
                        .agent_runtime
                        .resolve_prompt_assembly(dto)
                        .await
                        .map(|()| Value::Null)
                },
            )
            .await
        }
        "resolve_agent_persistent_state_metadata_update" => {
            invoke_dto(
                &request.args,
                |dto: AgentResolvePersistentStateMetadataUpdateDto| async {
                    state
                        .services
                        .agent_runtime
                        .resolve_persistent_state_metadata_update(dto)
                        .await
                        .map(|()| Value::Null)
                },
            )
            .await
        }
        "plan_agent_run_prune" => {
            invoke_dto(&request.args, |dto: AgentPlanRunPruneDto| async move {
                state.services.agent_run_history.plan_run_prune(dto).await
            })
            .await
        }
        "apply_agent_run_prune" => {
            invoke_dto(&request.args, |dto: AgentApplyRunPruneDto| async move {
                state.services.agent_run_history.apply_run_prune(dto).await
            })
            .await
        }
        "copy_agent_chat_persistent_states" => {
            invoke_dto(
                &request.args,
                |dto: AgentCopyChatPersistentStatesDto| async move {
                    state
                        .services
                        .chats
                        .copy_agent_persistent_states(
                            &AgentChatWorkspaceTarget {
                                chat_ref: dto.source_chat_ref,
                                stable_chat_id: dto.source_stable_chat_id,
                            },
                            &AgentChatWorkspaceTarget {
                                chat_ref: dto.target_chat_ref,
                                stable_chat_id: dto.target_stable_chat_id,
                            },
                        )
                        .await?;
                    Ok(Value::Null)
                },
            )
            .await
        }
        "prune_agent_chat_persistent_states" => {
            invoke_dto(
                &request.args,
                |dto: AgentPruneChatPersistentStatesDto| async move {
                    let (character_id, file_name) = match &dto.chat_ref {
                        AgentChatRef::Character {
                            character_id,
                            file_name,
                        } => (character_id.as_str(), file_name.as_str()),
                        AgentChatRef::Group { .. } => {
                            return Err(ApplicationError::ValidationError(
                                "agent.group_persistent_state_prune_unsupported".to_string(),
                            ));
                        }
                    };
                    let candidates = dto.candidate_state_ids.ok_or_else(|| {
                        ApplicationError::ValidationError(
                            "agent.persistent_state_prune_candidates_required".to_string(),
                        )
                    })?;
                    let payload = state
                        .services
                        .chats
                        .get_chat_payload(character_id, file_name)
                        .await?;
                    let target = AgentChatWorkspaceTarget {
                        chat_ref: dto.chat_ref,
                        stable_chat_id: dto.stable_chat_id,
                    };
                    let result = state
                        .services
                        .chats
                        .prune_agent_persistent_states(
                            &target,
                            AgentPersistentStatePruneRequest {
                                retained_state_ids: collect_agent_persistent_state_ids(&payload),
                                candidate_state_ids: candidates,
                            },
                        )
                        .await?;
                    Ok(json!({
                        "workspaceId": result.workspace_id,
                        "removedStateIds": result.removed_state_ids,
                    }))
                },
            )
            .await
        }
        "start_import_data_archive" => {
            let archive_path = string_arg(&request.args, "archive_path", "archivePath");
            let archive_is_temporary = request
                .args
                .get("archive_is_temporary")
                .or_else(|| request.args.get("archiveIsTemporary"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            state
                .services
                .data_archives
                .start_import(Path::new(archive_path), archive_is_temporary)
                .map(|job_id| json!(job_id))
                .map_err(|error| error.to_string())
        }
        "list_nas_data_archives" => list_nas_data_archives(&state.import_root).await,
        "start_import_nas_data_archive" => {
            let file_name = string_arg(&request.args, "file_name", "fileName");
            match resolve_nas_data_archive(&state.import_root, file_name).await {
                Ok(archive_path) => state
                    .services
                    .data_archives
                    .start_import(&archive_path, false)
                    .map(|job_id| json!(job_id))
                    .map_err(|error| error.to_string()),
                Err(error) => Err(error),
            }
        }
        "start_export_data_archive" => state
            .services
            .data_archives
            .start_export()
            .map(|job_id| json!(job_id))
            .map_err(|error| error.to_string()),
        "prepare_data_archive_import_target_path" => state
            .services
            .data_archives
            .prepare_incoming_import_archive_path()
            .map(|path| json!(path.to_string_lossy().to_string()))
            .map_err(|error| error.to_string()),
        "get_data_archive_job_status" => {
            let job_id = string_arg(&request.args, "job_id", "jobId");
            state
                .services
                .data_archives
                .get_status(job_id)
                .map_err(|error| error.to_string())
                .and_then(|status| serde_json::to_value(status).map_err(|error| error.to_string()))
        }
        "cancel_data_archive_job" => {
            let job_id = string_arg(&request.args, "job_id", "jobId");
            state
                .services
                .data_archives
                .cancel(job_id)
                .map(|()| Value::Null)
                .map_err(|error| error.to_string())
        }
        "save_export_data_archive" => {
            let job_id = string_arg(&request.args, "job_id", "jobId").to_string();
            match state.services.data_archives.save_export(job_id).await {
                Ok(path) => Ok(json!(path.to_string_lossy().to_string())),
                Err(error) => Err(error.to_string()),
            }
        }
        "cleanup_export_data_archive" => {
            let job_id = string_arg(&request.args, "job_id", "jobId");
            state
                .services
                .data_archives
                .cleanup_export(job_id)
                .map(|()| Value::Null)
                .map_err(|error| error.to_string())
        }
        "cleanup_web_archive_download" => {
            let path = string_arg(&request.args, "path", "path");
            cleanup_web_archive_download(&state, Path::new(path)).await
        }
        "finalize_export_data_archive_delivery" => {
            let job_id = string_arg(&request.args, "job_id", "jobId");
            let saved_path = request
                .args
                .get("saved_path")
                .or_else(|| request.args.get("savedPath"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            state
                .services
                .data_archives
                .finalize_export_delivery(job_id, saved_path)
                .map(|error| json!(error))
                .map_err(|error| error.to_string())
        }
        "export_user_backup_archive" => {
            let handle = string_arg(&request.args, "handle", "handle").to_string();
            let include_secrets = request
                .args
                .get("include_secrets")
                .or_else(|| request.args.get("includeSecrets"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            match state
                .services
                .data_archives
                .export_user_backup(handle, include_secrets)
                .await
            {
                Ok(result) => serde_json::to_value(result).map_err(|error| error.to_string()),
                Err(error) => Err(error.to_string()),
            }
        }
        "save_user_backup_archive" => {
            let archive_path = string_arg(&request.args, "archive_path", "archivePath").to_string();
            let file_name = string_arg(&request.args, "file_name", "fileName").to_string();
            match state
                .services
                .data_archives
                .save_user_backup(archive_path, file_name)
                .await
            {
                Ok(path) => Ok(json!(path.to_string_lossy().to_string())),
                Err(error) => Err(error.to_string()),
            }
        }
        "cleanup_user_backup_archive" => {
            let archive_path = string_arg(&request.args, "archive_path", "archivePath");
            state
                .services
                .data_archives
                .cleanup_user_backup(archive_path)
                .map(|()| Value::Null)
                .map_err(|error| error.to_string())
        }
        "list_agent_runs" => {
            invoke_dto(&request.args, |dto: AgentListRunsDto| async move {
                state.services.agent_run_history.list_runs(dto).await
            })
            .await
        }
        "subscribe_agent_run_live_projection" => {
            stream_agent_live_projection(&state, &request.args).await
        }
        "get_bootstrap_snapshot" => match build_bootstrap_snapshot(&state).await {
            Ok(snapshot) => serde_json::to_value(snapshot).map_err(|error| error.to_string()),
            Err(error) => Err(error.to_string()),
        },
        "get_sillytavern_settings" => state
            .services
            .settings
            .get_sillytavern_settings()
            .await
            .map_err(|error| error.to_string())
            .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string())),
        "get_tauritavern_settings" => state
            .services
            .settings
            .get_tauritavern_settings()
            .await
            .map_err(|error| error.to_string())
            .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string())),
        "list_llm_connections" => state
            .services
            .llm_connections
            .list_connections()
            .await
            .map(|connections| json!(ListLlmConnectionsResultDto { connections }))
            .map_err(|error| error.to_string()),
        "load_llm_connection" => match parse_dto::<LlmConnectionIdDto>(&request.args, "dto") {
            Ok(dto) => state
                .services
                .llm_connections
                .load_connection(&dto.connection_id)
                .await
                .map(|connection| json!(LoadLlmConnectionResultDto { connection }))
                .map_err(|error| error.to_string()),
            Err(error) => Err(error),
        },
        "save_llm_connection" => match parse_dto::<SaveLlmConnectionDto>(&request.args, "dto") {
            Ok(dto) => state
                .services
                .llm_connections
                .save_connection(dto.connection)
                .await
                .map(|()| Value::Null)
                .map_err(|error| error.to_string()),
            Err(error) => Err(error),
        },
        "delete_llm_connection" => match parse_dto::<LlmConnectionIdDto>(&request.args, "dto") {
            Ok(dto) => state
                .services
                .llm_connections
                .delete_connection(&dto.connection_id)
                .await
                .map(|()| Value::Null)
                .map_err(|error| error.to_string()),
            Err(error) => Err(error),
        },
        "save_preset" => match parse_dto::<SavePresetDto>(&request.args, "dto") {
            Ok(dto) => match state.services.presets.create_preset(
                dto.name.clone(),
                &dto.api_id,
                dto.preset,
            ) {
                Ok(preset) => state
                    .services
                    .presets
                    .save_preset(&preset)
                    .await
                    .map(|()| json!(SavePresetResponseDto::new(preset.name)))
                    .map_err(|error| error.to_string()),
                Err(error) => Err(error.to_string()),
            },
            Err(error) => Err(error),
        },
        "save_openai_preset" => {
            let name = string_arg(&request.args, "name", "name").to_string();
            match parse_dto::<SaveOpenAIPresetDto>(&request.args, "dto") {
                Ok(dto) => match state
                    .services
                    .presets
                    .create_preset(name, "openai", dto.preset)
                {
                    Ok(preset) => state
                        .services
                        .presets
                        .save_preset(&preset)
                        .await
                        .map(|()| json!(SavePresetResponseDto::new(preset.name)))
                        .map_err(|error| error.to_string()),
                    Err(error) => Err(error.to_string()),
                },
                Err(error) => Err(error),
            }
        }
        "delete_preset" => match parse_dto::<DeletePresetDto>(&request.args, "dto") {
            Ok(dto) => match PresetType::from_api_id(&dto.api_id) {
                Some(preset_type) => state
                    .services
                    .presets
                    .delete_preset(&dto.name, &preset_type)
                    .await
                    .map(|()| Value::Null)
                    .map_err(|error| error.to_string()),
                None => Err(format!("Unknown API ID: {}", dto.api_id)),
            },
            Err(error) => Err(error),
        },
        "delete_openai_preset" => match parse_dto::<DeleteOpenAIPresetDto>(&request.args, "dto") {
            Ok(dto) => {
                let deleted = state
                    .services
                    .presets
                    .delete_preset(&dto.name, &PresetType::OpenAI)
                    .await
                    .is_ok();
                Ok(json!(if deleted {
                    DeleteOpenAIPresetResponseDto::success()
                } else {
                    DeleteOpenAIPresetResponseDto::error()
                }))
            }
            Err(error) => Err(error),
        },
        "restore_preset" => match parse_dto::<RestorePresetDto>(&request.args, "dto") {
            Ok(dto) => match PresetType::from_api_id(&dto.api_id) {
                Some(preset_type) => state
                    .services
                    .presets
                    .restore_default_preset(&dto.name, &preset_type)
                    .await
                    .map(|preset| match preset {
                        Some(preset) => {
                            json!(RestorePresetResponseDto::new(true, preset.data))
                        }
                        None => json!(RestorePresetResponseDto::not_found()),
                    })
                    .map_err(|error| error.to_string()),
                None => Err(format!("Unknown API ID: {}", dto.api_id)),
            },
            Err(error) => Err(error),
        },
        "list_presets" => {
            let api_id = string_arg(&request.args, "api_id", "apiId");
            match PresetType::from_api_id(api_id) {
                Some(preset_type) => state
                    .services
                    .presets
                    .list_presets(&preset_type)
                    .await
                    .map(|names| json!(names))
                    .map_err(|error| error.to_string()),
                None => Err(format!("Unknown API ID: {api_id}")),
            }
        }
        "preset_exists" => {
            let name = string_arg(&request.args, "name", "name");
            let api_id = string_arg(&request.args, "api_id", "apiId");
            match PresetType::from_api_id(api_id) {
                Some(preset_type) => state
                    .services
                    .presets
                    .preset_exists(name, &preset_type)
                    .await
                    .map(|exists| json!(exists))
                    .map_err(|error| error.to_string()),
                None => Err(format!("Unknown API ID: {api_id}")),
            }
        }
        "get_preset" => {
            let name = string_arg(&request.args, "name", "name");
            let api_id = string_arg(&request.args, "api_id", "apiId");
            match PresetType::from_api_id(api_id) {
                Some(preset_type) => state
                    .services
                    .presets
                    .get_preset(name, &preset_type)
                    .await
                    .map(|preset| preset.map(|preset| preset.data).unwrap_or(Value::Null))
                    .map_err(|error| error.to_string()),
                None => Err(format!("Unknown API ID: {api_id}")),
            }
        }
        "save_user_settings" => match parse_dto::<UserSettingsDto>(&request.args, "settings") {
            Ok(dto) => state
                .services
                .settings
                .save_user_settings(dto)
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string())),
            Err(error) => Err(error),
        },
        "save_user_settings_patch" => {
            match parse_dto::<UserSettingsPatchDto>(&request.args, "patch") {
                Ok(dto) => state
                    .services
                    .settings
                    .save_user_settings_patch(dto)
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|value| {
                        serde_json::to_value(value).map_err(|error| error.to_string())
                    }),
                Err(error) => Err(error),
            }
        }
        "update_tauritavern_settings" => {
            match parse_dto::<UpdateTauriTavernSettingsDto>(&request.args, "dto") {
                Ok(dto) => match state
                    .services
                    .settings
                    .update_tauritavern_settings(dto)
                    .await
                {
                    Ok(value) => {
                        if let Ok(settings) =
                            state.services.settings.get_tauritavern_settings().await
                        {
                            state
                                .dev_logs
                                .set_llm_keep(settings.dev.llm_api_keep as usize);
                        }
                        serde_json::to_value(value).map_err(|error| error.to_string())
                    }
                    Err(error) => Err(error.to_string()),
                },
                Err(error) => Err(error),
            }
        }
        "read_secret_state" => state
            .services
            .secrets
            .read_secret_state()
            .await
            .map_err(|error| error.to_string())
            .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string())),
        "read_secret_settings" => serde_json::to_value(state.services.secrets.read_settings())
            .map_err(|error| error.to_string()),
        "view_secrets" => state
            .services
            .secrets
            .view_secrets()
            .await
            .map_err(|error| error.to_string())
            .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string())),
        "write_secret" => match parse_dto::<WriteSecretDto>(&request.args, "dto") {
            Ok(dto) => state
                .services
                .secrets
                .write_secret(&dto.key, &dto.value, dto.label.as_deref())
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string())),
            Err(error) => Err(error),
        },
        "find_secret" => match parse_dto::<FindSecretDto>(&request.args, "dto") {
            Ok(dto) => state
                .services
                .secrets
                .find_secret(&dto.key, dto.id.as_deref())
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string())),
            Err(error) => Err(error),
        },
        "delete_secret" => match parse_dto::<DeleteSecretDto>(&request.args, "dto") {
            Ok(dto) => state
                .services
                .secrets
                .delete_secret(&dto.key, dto.id.as_deref())
                .await
                .map(|()| Value::Null)
                .map_err(|error| error.to_string()),
            Err(error) => Err(error),
        },
        "rotate_secret" => match parse_dto::<RotateSecretDto>(&request.args, "dto") {
            Ok(dto) => state
                .services
                .secrets
                .rotate_secret(&dto.key, &dto.id)
                .await
                .map(|()| Value::Null)
                .map_err(|error| error.to_string()),
            Err(error) => Err(error),
        },
        "rename_secret" => match parse_dto::<RenameSecretDto>(&request.args, "dto") {
            Ok(dto) => state
                .services
                .secrets
                .rename_secret(&dto.key, &dto.id, &dto.label)
                .await
                .map(|()| Value::Null)
                .map_err(|error| error.to_string()),
            Err(error) => Err(error),
        },
        "get_chat_completions_status" => {
            let dto = match parse_dto::<ChatCompletionStatusRequestDto>(&request.args, "dto") {
                Ok(dto) => dto,
                Err(error) => return bad_request(error),
            };
            let endpoint = match state
                .services
                .chat_completion
                .resolve_status_user_endpoint(&dto)
            {
                Ok(endpoint) => endpoint,
                Err(error) => return application_error(error),
            };
            if let Some(endpoint) = endpoint
                && !state.services.endpoint_access.is_granted(&endpoint).await
            {
                state
                    .services
                    .pending_endpoint_grants
                    .lock()
                    .await
                    .insert(endpoint.clone());
                return endpoint_confirmation_required(endpoint);
            }
            state
                .services
                .chat_completion
                .get_status(dto)
                .await
                .map_err(|error| error.to_string())
        }
        "get_all_characters" => {
            let shallow = request
                .args
                .get("shallow")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            state
                .services
                .characters
                .get_all_characters(shallow)
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string()))
        }
        "get_character" => {
            let name = string_arg(&request.args, "name", "name");
            state
                .services
                .characters
                .get_character(name)
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string()))
        }
        "create_character" => match parse_dto::<CreateCharacterDto>(&request.args, "dto") {
            Ok(dto) => state
                .services
                .characters
                .create_character(dto)
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string())),
            Err(error) => Err(error),
        },
        "create_character_with_avatar" => {
            match parse_dto::<CreateWithAvatarDto>(&request.args, "dto") {
                Ok(dto) => state
                    .services
                    .characters
                    .create_with_avatar(dto)
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|value| {
                        serde_json::to_value(value).map_err(|error| error.to_string())
                    }),
                Err(error) => Err(error),
            }
        }
        "update_character" => {
            let name = string_arg(&request.args, "name", "name");
            match parse_dto::<UpdateCharacterDto>(&request.args, "dto") {
                Ok(dto) => state
                    .services
                    .characters
                    .update_character(name, dto)
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|value| {
                        serde_json::to_value(value).map_err(|error| error.to_string())
                    }),
                Err(error) => Err(error),
            }
        }
        "delete_character" => match parse_dto::<DeleteCharacterDto>(&request.args, "dto") {
            Ok(dto) => state
                .services
                .characters
                .delete_character(dto)
                .await
                .map(|()| Value::Null)
                .map_err(|error| error.to_string()),
            Err(error) => Err(error),
        },
        "update_character_card_data" => {
            let name = string_arg(&request.args, "name", "name");
            match parse_dto::<UpdateCharacterCardDataDto>(&request.args, "dto") {
                Ok(dto) => state
                    .services
                    .characters
                    .update_character_card_data(name, dto)
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|value| {
                        serde_json::to_value(value).map_err(|error| error.to_string())
                    }),
                Err(error) => Err(error),
            }
        }
        "check_character_lorebook_conflict" => {
            invoke_dto(
                &request.args,
                |dto: CheckCharacterLorebookConflictDto| async move {
                    state.services.characters.check_lorebook_conflict(dto).await
                },
            )
            .await
        }
        "resolve_character_lorebook_conflict" => {
            invoke_dto(
                &request.args,
                |dto: ResolveCharacterLorebookConflictDto| async move {
                    state
                        .services
                        .characters
                        .resolve_lorebook_conflict(dto)
                        .await
                },
            )
            .await
        }
        "merge_character_card_data" => {
            let name = string_arg(&request.args, "name", "name");
            match parse_dto::<MergeCharacterCardDataDto>(&request.args, "dto") {
                Ok(dto) => state
                    .services
                    .characters
                    .merge_character_card_data(name, dto)
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|value| {
                        serde_json::to_value(value).map_err(|error| error.to_string())
                    }),
                Err(error) => Err(error),
            }
        }
        "bulk_merge_character_card_data" => {
            invoke_dto(
                &request.args,
                |dto: BulkMergeCharacterCardDataDto| async move {
                    state
                        .services
                        .characters
                        .bulk_merge_character_card_data(dto)
                        .await
                },
            )
            .await
        }
        "rename_character" => match parse_dto::<RenameCharacterDto>(&request.args, "dto") {
            Ok(dto) => {
                let from_id = dto.old_name.clone();
                match state.services.characters.rename_character(dto).await {
                    Ok(renamed) => {
                        let retarget_result = if let Some(to_id) = renamed
                            .avatar
                            .strip_suffix(".png")
                            .filter(|id| !id.is_empty())
                        {
                            if from_id != to_id {
                                state
                                    .services
                                    .skills
                                    .retarget_scope(SkillScopeRetargetRequest {
                                        from_scope: SkillScope::Character {
                                            character_id: from_id,
                                        },
                                        to_scope: SkillScope::Character {
                                            character_id: to_id.to_string(),
                                        },
                                    })
                                    .await
                                    .map(|_| ())
                                    .map_err(|error| error.to_string())
                            } else {
                                Ok(())
                            }
                        } else {
                            Ok(())
                        };
                        match retarget_result {
                            Ok(()) => {
                                serde_json::to_value(renamed).map_err(|error| error.to_string())
                            }
                            Err(error) => Err(format!(
                                "Character was renamed, but linked Agent Skills could not be retargeted: {error}"
                            )),
                        }
                    }
                    Err(error) => Err(error.to_string()),
                }
            }
            Err(error) => Err(error),
        },
        "duplicate_character" => {
            invoke_dto(&request.args, |dto: DuplicateCharacterDto| async move {
                state.services.characters.duplicate_character(dto).await
            })
            .await
        }
        "replace_character" => match parse_dto::<ReplaceCharacterDto>(&request.args, "dto") {
            Ok(mut dto) => match resolve_staged_upload(&state, &dto.file_path).await {
                Ok(path) => {
                    dto.file_path = path.to_string_lossy().into_owned();
                    state
                        .services
                        .characters
                        .replace_character(dto)
                        .await
                        .map_err(|error| error.to_string())
                        .and_then(|value| {
                            serde_json::to_value(value).map_err(|error| error.to_string())
                        })
                }
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        },
        "export_character_content" => {
            match parse_dto::<ExportCharacterContentDto>(&request.args, "dto") {
                Ok(dto) => state
                    .services
                    .characters
                    .export_character_content(dto)
                    .await
                    .map(|value| json!(value))
                    .map_err(|error| error.to_string()),
                Err(error) => Err(error),
            }
        }
        "import_character" => match parse_dto::<ImportCharacterDto>(&request.args, "dto") {
            Ok(dto) => state
                .services
                .characters
                .import_character(dto)
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string())),
            Err(error) => Err(error),
        },
        "get_character_chats_by_id" => {
            let dto = parse_dto(&request.args, "dto");
            match dto {
                Ok(dto) => state
                    .services
                    .characters
                    .get_character_chats(dto)
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|value| {
                        serde_json::to_value(value).map_err(|error| error.to_string())
                    }),
                Err(error) => Err(error),
            }
        }
        "get_world_info" => match parse_dto::<GetWorldInfoDto>(&request.args, "dto") {
            Ok(dto) => state
                .services
                .world_info
                .get_world_info(&dto.name)
                .await
                .map_err(|error| error.to_string()),
            Err(error) => Err(error),
        },
        "get_world_infos_batch" => match parse_dto::<GetWorldInfosBatchDto>(&request.args, "dto") {
            Ok(dto) => state
                .services
                .world_info
                .get_world_infos_batch(dto.names)
                .await
                .map(|items| json!(GetWorldInfosBatchResponseDto { items }))
                .map_err(|error| error.to_string()),
            Err(error) => Err(error),
        },
        "save_world_info" => match parse_dto::<SaveWorldInfoDto>(&request.args, "dto") {
            Ok(dto) => state
                .services
                .world_info
                .save_world_info(&dto.name, dto.data)
                .await
                .map(|()| Value::Null)
                .map_err(|error| error.to_string()),
            Err(error) => Err(error),
        },
        "delete_world_info" => match parse_dto::<DeleteWorldInfoDto>(&request.args, "dto") {
            Ok(dto) => state
                .services
                .world_info
                .delete_world_info(&dto.name)
                .await
                .map(|()| Value::Null)
                .map_err(|error| error.to_string()),
            Err(error) => Err(error),
        },
        "import_world_info" => match parse_dto::<ImportWorldInfoDto>(&request.args, "dto") {
            Ok(dto) => {
                let has_converted_data = dto
                    .converted_data
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty());
                let path_is_staged = if dto.file_path.trim().is_empty() {
                    false
                } else {
                    let staging_root = state.data_root.join("_tauritavern").join("web-uploads");
                    match tokio::try_join!(
                        tokio::fs::canonicalize(staging_root),
                        tokio::fs::canonicalize(&dto.file_path),
                    ) {
                        Ok((root, file)) => file.starts_with(root) && file.is_file(),
                        Err(_) => false,
                    }
                };
                if !has_converted_data && !path_is_staged {
                    Err("World info import must use a staged browser upload".to_string())
                } else {
                    state
                        .services
                        .world_info
                        .import_world_info(
                            &dto.file_path,
                            &dto.original_filename,
                            dto.converted_data,
                        )
                        .await
                        .map(|name| json!(ImportWorldInfoResponseDto { name }))
                        .map_err(|error| error.to_string())
                }
            }
            Err(error) => Err(error),
        },
        "normalize_world_info_name" => {
            match parse_dto::<NormalizeWorldInfoNameDto>(&request.args, "dto") {
                Ok(dto) => state
                    .services
                    .world_info
                    .normalize_world_info_name(&dto.name, dto.import_filename)
                    .map(|name| json!({ "name": name }))
                    .map_err(|error| error.to_string()),
                Err(error) => Err(error),
            }
        }
        "get_chat_payload" => {
            let character = string_arg(&request.args, "character_name", "characterName");
            let file = string_arg(&request.args, "file_name", "fileName");
            state
                .services
                .chats
                .get_chat_payload(character, file)
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string()))
        }
        "get_character_chat_summary" => {
            let character = string_arg(&request.args, "character_name", "characterName");
            let file = string_arg(&request.args, "file_name", "fileName");
            let include_metadata = request
                .args
                .get("include_metadata")
                .or_else(|| request.args.get("includeMetadata"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            state
                .services
                .chats
                .get_character_chat_summary(character, file, include_metadata)
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string()))
        }
        "get_character_chat_metadata" => {
            let character = string_arg(&request.args, "character_name", "characterName");
            let file = string_arg(&request.args, "file_name", "fileName");
            state
                .services
                .chats
                .get_character_chat_metadata(character, file)
                .await
                .map_err(|error| error.to_string())
        }
        "set_character_chat_metadata_extension" => {
            let character = string_arg(&request.args, "character_name", "characterName");
            let file = string_arg(&request.args, "file_name", "fileName");
            let namespace = string_arg(&request.args, "namespace", "namespace");
            let value = request.args.get("value").cloned().unwrap_or(Value::Null);
            state
                .services
                .chats
                .set_character_chat_metadata_extension(character, file, namespace, value)
                .await
                .map(|()| Value::Null)
                .map_err(|error| error.to_string())
        }
        "get_all_groups" => state
            .services
            .groups
            .get_all_groups()
            .await
            .map(|groups| json!(groups.into_iter().map(GroupDto::from).collect::<Vec<_>>()))
            .map_err(|error| error.to_string()),
        "get_group" => {
            let id = string_arg(&request.args, "id", "id");
            state
                .services
                .groups
                .get_group(id)
                .await
                .map(|group| json!(group.map(GroupDto::from)))
                .map_err(|error| error.to_string())
        }
        "create_group" => match parse_dto::<CreateGroupDto>(&request.args, "dto") {
            Ok(dto) => state
                .services
                .groups
                .create_group(dto)
                .await
                .map(|group| json!(GroupDto::from(group)))
                .map_err(|error| error.to_string()),
            Err(error) => Err(error),
        },
        "update_group" => match parse_dto::<UpdateGroupDto>(&request.args, "dto") {
            Ok(dto) => state
                .services
                .groups
                .update_group(dto)
                .await
                .map(|group| json!(GroupDto::from(group)))
                .map_err(|error| error.to_string()),
            Err(error) => Err(error),
        },
        "delete_group" => match parse_dto::<DeleteGroupDto>(&request.args, "dto") {
            Ok(dto) => state
                .services
                .groups
                .delete_group(dto)
                .await
                .map(|()| Value::Null)
                .map_err(|error| error.to_string()),
            Err(error) => Err(error),
        },
        "get_group_chat_paths" => state
            .services
            .groups
            .get_group_chat_paths()
            .await
            .map(|paths| json!(paths))
            .map_err(|error| error.to_string()),
        "clear_group_cache" => state
            .services
            .groups
            .clear_cache()
            .await
            .map(|()| Value::Null)
            .map_err(|error| error.to_string()),
        "get_group_chat_summary" => {
            let id = string_arg(&request.args, "chat_id", "chatId");
            let include_metadata = request
                .args
                .get("include_metadata")
                .or_else(|| request.args.get("includeMetadata"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            state
                .services
                .group_chats
                .get_group_chat_summary(id, include_metadata)
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string()))
        }
        "get_group_chat_metadata" => {
            let id = string_arg(&request.args, "chat_id", "chatId");
            state
                .services
                .group_chats
                .get_group_chat_metadata(id)
                .await
                .map_err(|error| error.to_string())
        }
        "set_group_chat_metadata_extension" => {
            let id = string_arg(&request.args, "chat_id", "chatId");
            let namespace = string_arg(&request.args, "namespace", "namespace");
            let value = request.args.get("value").cloned().unwrap_or(Value::Null);
            state
                .services
                .group_chats
                .set_group_chat_metadata_extension(id, namespace, value)
                .await
                .map(|()| Value::Null)
                .map_err(|error| error.to_string())
        }
        "list_chat_summaries" => {
            let filter = request
                .args
                .get("character_filter")
                .or_else(|| request.args.get("characterFilter"))
                .and_then(Value::as_str);
            let include_metadata = request
                .args
                .get("include_metadata")
                .or_else(|| request.args.get("includeMetadata"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            state
                .services
                .chats
                .list_chat_summaries(filter, include_metadata)
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string()))
        }
        "get_chat_payload_path" => {
            let character = string_arg(&request.args, "character_name", "characterName");
            let file = string_arg(&request.args, "file_name", "fileName");
            state
                .services
                .chats
                .get_chat_payload_path(character, file)
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string()))
        }
        "get_group_chat_path" => {
            let id = string_arg(&request.args, "id", "id");
            state
                .services
                .group_chats
                .get_group_chat_payload_path(id)
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string()))
        }
        "create_chat" => match parse_dto(&request.args, "dto") {
            Ok(dto) => state
                .services
                .chats
                .create_chat(dto)
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string())),
            Err(error) => Err(error),
        },
        "add_message" => match parse_dto(&request.args, "dto") {
            Ok(dto) => state
                .services
                .chats
                .add_message(dto)
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string())),
            Err(error) => Err(error),
        },
        "rename_chat" => match parse_dto(&request.args, "dto") {
            Ok(dto) => state
                .services
                .chats
                .rename_chat(dto)
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string())),
            Err(error) => Err(error),
        },
        "delete_chat" => {
            let character = string_arg(&request.args, "character_name", "characterName");
            let file = string_arg(&request.args, "file_name", "fileName");
            state
                .services
                .chats
                .delete_chat(character, file)
                .await
                .map(|()| Value::Null)
                .map_err(|error| error.to_string())
        }
        "begin_chat_commit" => {
            let target = match parse_dto::<ChatHistoryLocator>(&request.args, "target") {
                Ok(target) => target,
                Err(error) => return bad_request(error),
            };
            let force = request
                .args
                .get("force")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            state
                .services
                .chat_commits
                .begin(target, force, None)
                .await
                .map(|result| {
                    json!(BeginCommitResponse {
                        session_id: result.session_id,
                        max_frame_bytes: result.max_frame_bytes,
                    })
                })
                .map_err(|error| error.to_string())
        }
        "commit_chat_metadata" => {
            let target = match parse_dto::<ChatHistoryLocator>(&request.args, "target") {
                Ok(target) => target,
                Err(error) => return bad_request(error),
            };
            let metadata = request
                .args
                .get("chat_metadata")
                .or_else(|| request.args.get("chatMetadata"))
                .cloned()
                .unwrap_or(Value::Null);
            state
                .services
                .chat_commits
                .commit_metadata(target, metadata)
                .await
                .map(|()| Value::Null)
                .map_err(|error| error.to_string())
        }
        "finish_chat_commit" => {
            let session_id = string_arg(&request.args, "session_id", "sessionId");
            let expected_size = request
                .args
                .get("expected_size")
                .or_else(|| request.args.get("expectedSize"))
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let commit_reason = match request
                .args
                .get("commit_reason")
                .or_else(|| request.args.get("commitReason"))
            {
                None => Ok(CurrentCommitReason::Mutation),
                Some(value) => serde_json::from_value::<CurrentCommitReason>(value.clone())
                    .map_err(|error| error.to_string()),
            };
            match commit_reason {
                Ok(reason) => state
                    .services
                    .chat_commits
                    .finish(session_id, expected_size, reason)
                    .await
                    .map(|result| {
                        json!(FinishCommitResponse {
                            accepted_size: result.accepted_size,
                            size: result.size,
                        })
                    })
                    .map_err(|error| error.to_string()),
                Err(error) => Err(error),
            }
        }
        "abort_chat_commit" => {
            let session_id = string_arg(&request.args, "session_id", "sessionId");
            state
                .services
                .chat_commits
                .abort(session_id)
                .await
                .map(|()| Value::Null)
                .map_err(|error| error.to_string())
        }
        "start_web_generation" => {
            let dto = match parse_dto(&request.args, "dto") {
                Ok(dto) => dto,
                Err(error) => return bad_request(error),
            };
            let task_id = string_arg(&request.args, "task_id", "taskId").to_string();
            let chat_key = string_arg(&request.args, "chat_key", "chatKey").to_string();
            let revision = string_arg(&request.args, "revision", "revision").to_string();
            let endpoint = match state
                .services
                .chat_completion
                .resolve_generate_user_endpoint(&dto)
            {
                Ok(endpoint) => endpoint,
                Err(error) => return application_error(error),
            };
            if let Some(endpoint) = endpoint
                && !state.services.endpoint_access.is_granted(&endpoint).await
            {
                state
                    .services
                    .pending_endpoint_grants
                    .lock()
                    .await
                    .insert(endpoint.clone());
                return endpoint_confirmation_required(endpoint);
            }
            state
                .web_generations
                .start(task_id, chat_key, revision, dto)
                .await
        }
        "read_web_generation" => {
            let task_id = string_arg(&request.args, "task_id", "taskId");
            let after_seq = request
                .args
                .get("after_seq")
                .or_else(|| request.args.get("afterSeq"))
                .and_then(Value::as_u64)
                .unwrap_or(0);
            state.web_generations.read(task_id, after_seq).await
        }
        "list_web_generations" => {
            let chat_key = string_arg(&request.args, "chat_key", "chatKey");
            Ok(state.web_generations.list(chat_key).await)
        }
        "cancel_web_generation" => {
            let task_id = string_arg(&request.args, "task_id", "taskId");
            state.web_generations.cancel(task_id).await
        }
        "claim_web_generation" => {
            let task_id = string_arg(&request.args, "task_id", "taskId");
            let token = string_arg(&request.args, "token", "token");
            state.web_generations.claim(task_id, token).await
        }
        "confirm_web_generation" => {
            let task_id = string_arg(&request.args, "task_id", "taskId");
            let token = string_arg(&request.args, "token", "token");
            state
                .web_generations
                .confirm_with_token(task_id, token)
                .await
        }
        "start_chat_completion_stream" => {
            let dto = match parse_dto(&request.args, "dto") {
                Ok(dto) => dto,
                Err(error) => return bad_request(error),
            };
            let stream_id = string_arg(&request.args, "stream_id", "streamId").to_string();
            let endpoint = match state
                .services
                .chat_completion
                .resolve_generate_user_endpoint(&dto)
            {
                Ok(endpoint) => endpoint,
                Err(error) => return application_error(error),
            };
            if let Some(endpoint) = endpoint
                && !state.services.endpoint_access.is_granted(&endpoint).await
            {
                state
                    .services
                    .pending_endpoint_grants
                    .lock()
                    .await
                    .insert(endpoint.clone());
                return endpoint_confirmation_required(endpoint);
            }
            state
                .services
                .chat_completion
                .clone()
                .start_stream(stream_id, dto)
                .await
                .map(|()| Value::Null)
                .map_err(|error| error.to_string())
        }
        "generate_chat_completion" => {
            let dto = match parse_dto::<ChatCompletionGenerateRequestDto>(&request.args, "dto") {
                Ok(dto) => dto,
                Err(error) => return bad_request(error),
            };
            let endpoint = match state
                .services
                .chat_completion
                .resolve_generate_user_endpoint(&dto)
            {
                Ok(endpoint) => endpoint,
                Err(error) => return application_error(error),
            };
            if let Some(endpoint) = endpoint
                && !state.services.endpoint_access.is_granted(&endpoint).await
            {
                state
                    .services
                    .pending_endpoint_grants
                    .lock()
                    .await
                    .insert(endpoint.clone());
                return endpoint_confirmation_required(endpoint);
            }
            let request_id = string_arg(&request.args, "request_id", "requestId");
            state
                .services
                .chat_completion
                .generate_request(request_id, dto)
                .await
                .map_err(|error| error.to_string())
        }
        "read_chat_completion_stream" => {
            let stream_id = string_arg(&request.args, "stream_id", "streamId");
            let after_seq = request
                .args
                .get("after_seq")
                .or_else(|| request.args.get("afterSeq"))
                .and_then(Value::as_u64)
                .unwrap_or(0);
            state
                .services
                .chat_completion
                .read_stream(stream_id, after_seq)
                .await
                .map_err(|error| error.to_string())
                .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string()))
        }
        "close_chat_completion_stream" => {
            let stream_id = string_arg(&request.args, "stream_id", "streamId");
            Ok(json!(
                state.services.chat_completion.close_stream(stream_id).await
            ))
        }
        "cancel_chat_completion_generation" => {
            let request_id = string_arg(&request.args, "request_id", "requestId");
            Ok(json!(
                state
                    .services
                    .chat_completion
                    .cancel_generation(request_id)
                    .await
            ))
        }
        "grant_user_endpoint_access" => {
            let endpoint = string_arg(&request.args, "endpoint", "endpoint").to_string();
            if !state
                .services
                .pending_endpoint_grants
                .lock()
                .await
                .remove(&endpoint)
            {
                return (StatusCode::FORBIDDEN, axum::Json(json!({ "error": "Endpoint authorization was not requested by the server" }))).into_response();
            }
            state.services.endpoint_access.grant(endpoint).await;
            Ok(Value::Null)
        }
        "chat_history_generation_started" | "chat_history_generation_finished" => {
            let locator = match parse_dto::<ChatHistoryLocator>(&request.args, "locator") {
                Ok(locator) => locator,
                Err(error) => return bad_request(error),
            };
            let result = if request.command == "chat_history_generation_started" {
                state
                    .services
                    .chat_history
                    .generation_started(locator)
                    .await
            } else {
                state
                    .services
                    .chat_history
                    .generation_finished(locator)
                    .await
            };
            result
                .map(|()| Value::Null)
                .map_err(|error| error.to_string())
        }
        "database_handle" => match request.args.get("request").cloned() {
            Some(request) => {
                match serde_json::from_value::<tt_contracts::database::DatabaseRequest>(request) {
                    Ok(request) => state
                        .services
                        .database
                        .execute(request)
                        .await
                        .map_err(|error| error.to_string())
                        .and_then(|response| {
                            serde_json::to_value(response).map_err(|error| error.to_string())
                        }),
                    Err(error) => Err(format!("Invalid database request: {error}")),
                }
            }
            None => Err("Database request is required".to_string()),
        },
        _ => return (
            StatusCode::NOT_IMPLEMENTED,
            axum::Json(
                json!({ "error": format!("Web command is not implemented: {}", request.command) }),
            ),
        )
            .into_response(),
    };
    if let Err(error) = &result {
        state_for_logs.dev_logs.record_backend(
            "ERROR",
            "web.invoke",
            &format!("{}: {error}", request.command),
        );
        state_for_logs.dev_logs.emit_or_queue_backend_error(error);
    }
    match result {
        Ok(result) => (StatusCode::OK, axum::Json(json!({ "result": result }))).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(json!({ "error": error })),
        )
            .into_response(),
    }
}

async fn invoke_binary(
    State(state): State<Arc<WebHostState>>,
    Query(query): Query<BinaryInvokeQuery>,
    request: Request<Body>,
) -> Response<Body> {
    let (parts, body) = request.into_parts();
    if query.command != "append_chat_commit_chunk" {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Some(session_id) = parts
        .headers
        .get("session-id")
        .and_then(|value| value.to_str().ok())
    else {
        return bad_request("Missing chat commit session-id header".to_string());
    };
    let Some(offset) = parts
        .headers
        .get("offset")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
    else {
        return bad_request("Invalid chat commit offset header".to_string());
    };
    let bytes = match axum::body::to_bytes(body, 8 * 1024 * 1024).await {
        Ok(bytes) => bytes,
        Err(error) => return bad_request(format!("Invalid chat commit chunk: {error}")),
    };
    match state
        .services
        .chat_commits
        .append(session_id, offset, &bytes)
        .await
    {
        Ok(next_offset) => {
            (StatusCode::OK, axum::Json(json!({ "result": next_offset }))).into_response()
        }
        Err(error) => application_error(error),
    }
}

fn bad_request(error: String) -> Response<Body> {
    (
        StatusCode::BAD_REQUEST,
        axum::Json(json!({ "error": error })),
    )
        .into_response()
}

fn application_error(error: ApplicationError) -> Response<Body> {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        axum::Json(json!({ "error": error.to_string() })),
    )
        .into_response()
}

fn application_error_message(error: String) -> Response<Body> {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        axum::Json(json!({ "error": error })),
    )
        .into_response()
}

fn endpoint_confirmation_required(endpoint: String) -> Response<Body> {
    (
        StatusCode::FORBIDDEN,
        axum::Json(json!({
            "error": "Custom endpoint authorization required",
            "requires_confirmation": true,
            "endpoint": endpoint,
        })),
    )
        .into_response()
}

fn string_arg<'a>(args: &'a Value, snake: &str, camel: &str) -> &'a str {
    args.get(snake)
        .or_else(|| args.get(camel))
        .and_then(Value::as_str)
        .unwrap_or_default()
}

async fn list_nas_data_archives(import_root: &Path) -> Result<Value, String> {
    let canonical_root = tokio::fs::canonicalize(import_root)
        .await
        .map_err(|error| format!("NAS import folder is unavailable: {error}"))?;
    let mut entries = tokio::fs::read_dir(&canonical_root)
        .await
        .map_err(|error| format!("Unable to read NAS import folder: {error}"))?;
    let mut archives = Vec::new();

    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|error| format!("Unable to read NAS import folder: {error}"))?
    {
        let file_type = entry
            .file_type()
            .await
            .map_err(|error| format!("Unable to inspect NAS import file: {error}"))?;
        if !file_type.is_file() {
            continue;
        }
        let Some(file_name) = entry.file_name().to_str().map(ToOwned::to_owned) else {
            continue;
        };
        if !is_data_archive_name(&file_name) {
            continue;
        }
        let metadata = entry
            .metadata()
            .await
            .map_err(|error| format!("Unable to inspect NAS import file: {error}"))?;
        archives.push(json!({
            "fileName": file_name,
            "sizeBytes": metadata.len(),
        }));
    }

    archives.sort_by(|left, right| {
        left.get("fileName")
            .and_then(Value::as_str)
            .cmp(&right.get("fileName").and_then(Value::as_str))
    });
    Ok(json!(archives))
}

async fn resolve_nas_data_archive(import_root: &Path, file_name: &str) -> Result<PathBuf, String> {
    let name = Path::new(file_name);
    if file_name.trim().is_empty()
        || name.file_name().and_then(|value| value.to_str()) != Some(file_name)
        || !is_data_archive_name(file_name)
    {
        return Err("Invalid NAS archive file name".to_string());
    }

    let canonical_root = tokio::fs::canonicalize(import_root)
        .await
        .map_err(|error| format!("NAS import folder is unavailable: {error}"))?;
    let candidate = tokio::fs::canonicalize(canonical_root.join(name))
        .await
        .map_err(|error| format!("NAS archive file was not found: {error}"))?;
    if !candidate.starts_with(&canonical_root) {
        return Err("NAS archive path is outside the import folder".to_string());
    }
    let metadata = tokio::fs::metadata(&candidate)
        .await
        .map_err(|error| format!("Unable to inspect NAS archive: {error}"))?;
    if !metadata.is_file() {
        return Err("NAS archive must be a regular file".to_string());
    }
    Ok(candidate)
}

fn is_data_archive_name(file_name: &str) -> bool {
    let lower = file_name.to_ascii_lowercase();
    [".zip", ".tar", ".tar.gz", ".tgz", ".gz"]
        .iter()
        .any(|extension| lower.ends_with(extension))
}

fn parse_dto<T: serde::de::DeserializeOwned>(args: &Value, key: &str) -> Result<T, String> {
    let value = args.get(key).cloned().unwrap_or(Value::Null);
    serde_json::from_value(value).map_err(|error| format!("Invalid {key}: {error}"))
}

fn parse_args<T: serde::de::DeserializeOwned>(args: &Value) -> Result<T, String> {
    serde_json::from_value(args.clone())
        .map_err(|error| format!("Invalid command arguments: {error}"))
}

async fn resolve_staged_upload(
    state: &WebHostState,
    supplied_path: &str,
) -> Result<PathBuf, String> {
    let staging_root = state.data_root.join("_tauritavern").join("web-uploads");
    let root = tokio::fs::canonicalize(staging_root)
        .await
        .map_err(|error| format!("Upload staging directory is unavailable: {error}"))?;
    let path = tokio::fs::canonicalize(supplied_path)
        .await
        .map_err(|error| format!("Uploaded file is unavailable: {error}"))?;
    let metadata = tokio::fs::metadata(&path)
        .await
        .map_err(|error| format!("Uploaded file cannot be inspected: {error}"))?;
    if !path.starts_with(root) || !metadata.is_file() {
        return Err("Upload path is outside the web upload staging directory".to_string());
    }
    Ok(path)
}

fn parse_byte_array(args: &Value, snake: &str, camel: &str) -> Result<Vec<u8>, String> {
    let value = args
        .get(snake)
        .or_else(|| args.get(camel))
        .ok_or_else(|| format!("Missing upload data: {snake}"))?;
    if let Some(encoded) = value.as_str() {
        return BASE64_STANDARD
            .decode(encoded)
            .map_err(|error| format!("Invalid base64 upload data: {error}"));
    }
    value
        .as_array()
        .ok_or_else(|| "Upload data must be a byte array or base64 string".to_string())?
        .iter()
        .map(|byte| {
            byte.as_u64()
                .filter(|value| *value <= 255)
                .map(|value| value as u8)
                .ok_or_else(|| "Upload data contains an invalid byte".to_string())
        })
        .collect()
}

async fn save_user_file(
    state: &WebHostState,
    name: &str,
    bytes: Vec<u8>,
) -> Result<String, String> {
    const MAX_USER_FILE_BYTES: usize = 128 * 1024 * 1024;
    if bytes.is_empty() || bytes.len() > MAX_USER_FILE_BYTES {
        return Err("Upload data must be between 1 byte and 128 MiB".to_string());
    }
    if name.is_empty()
        || name.starts_with('.')
        || !name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
    {
        return Err("Invalid user file name".to_string());
    }
    let extension = Path::new(name)
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| format!(".{}", value.to_ascii_lowercase()))
        .unwrap_or_default();
    const UNSAFE_EXTENSIONS: &[&str] = &[
        ".php",
        ".exe",
        ".com",
        ".dll",
        ".pif",
        ".application",
        ".gadget",
        ".msi",
        ".jar",
        ".cmd",
        ".bat",
        ".reg",
        ".sh",
        ".py",
        ".js",
        ".jse",
        ".jsp",
        ".pdf",
        ".html",
        ".htm",
        ".hta",
        ".vb",
        ".vbs",
        ".vbe",
        ".cpl",
        ".msc",
        ".scr",
        ".sql",
        ".iso",
        ".img",
        ".dmg",
        ".ps1",
        ".ps1xml",
        ".ps2",
        ".ps2xml",
        ".psc1",
        ".psc2",
        ".msh",
        ".msh1",
        ".msh2",
        ".mshxml",
        ".msh1xml",
        ".msh2xml",
        ".scf",
        ".lnk",
        ".inf",
        ".doc",
        ".docm",
        ".docx",
        ".dot",
        ".dotm",
        ".dotx",
        ".xls",
        ".xlsm",
        ".xlsx",
        ".xlt",
        ".xltm",
        ".xltx",
        ".xlam",
        ".ppt",
        ".pptm",
        ".pptx",
        ".pot",
        ".potm",
        ".potx",
        ".ppam",
        ".ppsx",
        ".ppsm",
        ".pps",
        ".sldx",
        ".sldm",
        ".ws",
    ];
    if UNSAFE_EXTENSIONS.contains(&extension.as_str()) {
        return Err("Forbidden file extension".to_string());
    }
    let files_dir = state.data_root.join("default-user").join("files");
    tokio::fs::create_dir_all(&files_dir)
        .await
        .map_err(|error| error.to_string())?;
    tokio::fs::write(files_dir.join(name), bytes)
        .await
        .map_err(|error| error.to_string())?;
    Ok(format!("/user/files/{name}"))
}

async fn resolve_user_file(state: &WebHostState, input: &str) -> Result<PathBuf, String> {
    let value = if let Ok(url) = url::Url::parse(input) {
        url.path().to_string()
    } else {
        input.to_string()
    };
    let normalized = value.replace('\\', "/");
    let relative = normalized
        .trim_start_matches('/')
        .strip_prefix("user/files/")
        .ok_or_else(|| "Invalid user file path".to_string())?;
    let components = Path::new(relative).components().collect::<Vec<_>>();
    if components.is_empty()
        || components
            .iter()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err("Invalid user file path".to_string());
    }
    let root = tokio::fs::canonicalize(state.data_root.join("default-user").join("files"))
        .await
        .map_err(|error| error.to_string())?;
    let target = tokio::fs::canonicalize(root.join(relative))
        .await
        .map_err(|error| error.to_string())?;
    if !target.starts_with(&root) || !target.is_file() {
        return Err("Invalid or missing user file".to_string());
    }
    Ok(target)
}

fn collect_agent_persistent_state_ids(payload: &[Value]) -> Vec<String> {
    let mut retained = BTreeSet::new();
    for item in payload {
        collect_agent_persistent_state_id_from_extra(item.get("extra"), &mut retained);
        if let Some(swipe_info) = item.get("swipe_info").and_then(Value::as_array) {
            for swipe in swipe_info {
                collect_agent_persistent_state_id_from_extra(swipe.get("extra"), &mut retained);
            }
        }
    }
    retained.into_iter().collect()
}

fn collect_agent_persistent_state_id_from_extra(
    extra: Option<&Value>,
    retained: &mut BTreeSet<String>,
) {
    if let Some(state_id) = extra
        .and_then(|value| value.get("tauritavern"))
        .and_then(|value| value.get("agent"))
        .and_then(|value| value.get("persistStateId"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        retained.insert(state_id.to_string());
    }
}

async fn invoke_dto<T, F, Fut, R>(args: &Value, handler: F) -> Result<Value, String>
where
    T: serde::de::DeserializeOwned,
    F: FnOnce(T) -> Fut,
    Fut: std::future::Future<Output = Result<R, ApplicationError>>,
    R: serde::Serialize,
{
    let dto = parse_dto(args, "dto")?;
    let value = handler(dto).await.map_err(|error| error.to_string())?;
    serde_json::to_value(value).map_err(|error| error.to_string())
}

async fn cleanup_web_archive_download(state: &WebHostState, path: &Path) -> Result<Value, String> {
    let download_root = state.archive_root.join("downloads");
    let (root, file) = tokio::try_join!(
        tokio::fs::canonicalize(download_root),
        tokio::fs::canonicalize(path),
    )
    .map_err(|error| error.to_string())?;
    if !file.starts_with(root) || !file.is_file() {
        return Err(
            "Archive download path is outside the temporary downloads directory".to_string(),
        );
    }
    tokio::fs::remove_file(file)
        .await
        .map_err(|error| error.to_string())?;
    Ok(Value::Null)
}

async fn stream_agent_live_projection(state: &WebHostState, args: &Value) -> Result<Value, String> {
    let dto: AgentSubscribeRunLiveProjectionDto = parse_dto(args, "dto")?;
    let channel_id = args
        .get("channel")
        .and_then(|channel| channel.get("__ttWebChannel"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| "Agent live channel is missing".to_string())?;
    let sender = state
        .channels
        .lock()
        .await
        .get(channel_id)
        .cloned()
        .ok_or_else(|| "Agent live channel is disconnected".to_string())?;
    let Some(mut updates) = state
        .services
        .agent_runtime
        .subscribe_live_projection(&dto.run_id)
        .await
        .map_err(|error| error.to_string())?
    else {
        let snapshot = live_projection_snapshot(
            &tt_application::services::agent_runtime_service::AgentRunLiveProjection::default(),
        )?;
        let _ = sender.send(snapshot);
        return Ok(Value::Null);
    };
    tokio::spawn(async move {
        let mut disconnect_check = tokio::time::interval(Duration::from_secs(1));
        loop {
            let projection = updates.borrow_and_update().clone();
            let Ok(snapshot) = live_projection_snapshot(&projection) else {
                break;
            };
            if sender.send(snapshot).is_err() {
                break;
            }
            tokio::select! {
                changed = updates.changed() => if changed.is_err() { break },
                _ = disconnect_check.tick() => if sender.is_closed() { break },
            }
        }
    });
    Ok(Value::Null)
}

fn live_projection_snapshot(
    projection: &tt_application::services::agent_runtime_service::AgentRunLiveProjection,
) -> Result<Value, String> {
    use tt_application::{
        dto::agent_dto::{
            AgentRunLiveReasoningDto, AgentRunLiveToolCallDto, AgentRunLiveUpdateDto,
        },
        services::agent_runtime_service::ToolCallProjection,
    };
    use tt_domain::text_metrics::TextMetrics;

    let calls = projection
        .calls
        .iter()
        .map(|(key, call)| match &call.projection {
            ToolCallProjection::WriteFile { path, content } => AgentRunLiveToolCallDto::WriteFile {
                invocation_id: key.invocation_id.clone(),
                invocation_exit_policy: call.invocation_exit_policy,
                tool_call_index: key.tool_call_index,
                path: path.clone(),
                content: content.clone(),
                content_words: TextMetrics::from_text(content).words,
            },
            ToolCallProjection::ApplyPatch {
                path,
                old_string,
                new_string,
            } => AgentRunLiveToolCallDto::ApplyPatch {
                invocation_id: key.invocation_id.clone(),
                invocation_exit_policy: call.invocation_exit_policy,
                tool_call_index: key.tool_call_index,
                path: path.clone(),
                old_string: old_string.clone(),
                old_string_words: TextMetrics::from_text(old_string).words,
                new_string: new_string.clone(),
                new_string_words: TextMetrics::from_text(new_string).words,
            },
        })
        .collect();
    let reasoning = projection
        .reasoning
        .iter()
        .map(|(invocation_id, reasoning)| AgentRunLiveReasoningDto {
            invocation_id: invocation_id.clone(),
            invocation_exit_policy: reasoning.invocation_exit_policy,
            text: reasoning.text.clone(),
            tool_ids: reasoning.tool_ids.clone(),
        })
        .collect();
    serde_json::to_value(AgentRunLiveUpdateDto::Snapshot { calls, reasoning })
        .map_err(|error| error.to_string())
}

async fn build_bootstrap_snapshot(
    state: &WebHostState,
) -> Result<BootstrapSnapshotDto, ApplicationError> {
    let (settings, characters, groups, avatars, secret_state) = tokio::join!(
        state.services.settings.get_sillytavern_settings(),
        state.services.characters.get_all_characters(true),
        state.services.groups.get_all_groups(),
        state.services.avatars.get_avatars(),
        state.services.secrets.read_secret_state(),
    );
    let settings = settings?;
    let characters = characters?;
    Ok(BootstrapSnapshotDto {
        settings,
        characters,
        groups: groups
            .unwrap_or_default()
            .into_iter()
            .map(GroupDto::from)
            .collect(),
        avatars: avatars.unwrap_or_default(),
        secret_state: secret_state.unwrap_or_default(),
        ios_policy: state.services.ios_policy.clone(),
    })
}

async fn serve_host_resource(
    State(state): State<Arc<WebHostState>>,
    request: Request<Body>,
) -> Response<Body> {
    if request.uri().path().starts_with("/__tt/") {
        return (
            StatusCode::NOT_IMPLEMENTED,
            axum::Json(json!({
                "error": "The browser command runtime has not been initialized",
            })),
        )
            .into_response();
    }

    let (parts, _) = request.into_parts();
    if !matches!(parts.method, Method::GET | Method::HEAD | Method::OPTIONS) {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }

    let mut builder = http::Request::builder().method(parts.method).uri(parts.uri);
    if let Some(headers) = builder.headers_mut() {
        *headers = parts.headers;
    }
    let request = match builder.body(Vec::new()) {
        Ok(request) => request,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let response = state
        .host_resources
        .serve(&request, HostResourceDeliveryCapabilities::new(true, false));
    let (parts, bytes) = response.into_parts();
    let mut builder = Response::builder().status(parts.status);
    if let Some(headers) = builder.headers_mut() {
        *headers = parts.headers;
    }
    builder
        .body(Body::from(bytes))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

struct WebDataArchiveFileGateway {
    data_root: PathBuf,
    archive_root: PathBuf,
}

impl WebDataArchiveFileGateway {
    fn new(data_root: PathBuf, archive_root: PathBuf) -> Self {
        Self {
            data_root,
            archive_root,
        }
    }

    fn archive_root(&self) -> PathBuf {
        self.archive_root.clone()
    }

    fn export_root(&self) -> PathBuf {
        self.archive_root().join("exports")
    }

    fn ensure_archive_child(&self, path: &Path) -> Result<(), DomainError> {
        if path.starts_with(self.archive_root()) {
            Ok(())
        } else {
            Err(DomainError::InvalidData(
                "Archive path must stay inside the web archive directory".to_string(),
            ))
        }
    }

    fn download_target(&self, file_name: &str) -> Result<PathBuf, DomainError> {
        let name = Path::new(file_name)
            .file_name()
            .and_then(|value| value.to_str())
            .filter(|value| !value.is_empty() && *value == file_name)
            .ok_or_else(|| DomainError::InvalidData("Invalid archive file name".to_string()))?;
        let downloads = self.archive_root().join("downloads");
        std::fs::create_dir_all(&downloads)
            .map_err(|error| DomainError::InternalError(error.to_string()))?;
        let unique_name = format!(
            "{}-{}.zip",
            uuid::Uuid::new_v4().simple(),
            name.trim_end_matches(".zip")
        );
        Ok(downloads.join(unique_name))
    }
}

impl DataArchiveFileGateway for WebDataArchiveFileGateway {
    fn prepare_incoming_import_archive_path(&self) -> Result<PathBuf, DomainError> {
        let incoming = self.archive_root().join("incoming");
        std::fs::create_dir_all(&incoming)
            .map_err(|error| DomainError::InternalError(error.to_string()))?;
        Ok(incoming.join(format!("import-{}.zip", uuid::Uuid::new_v4().simple())))
    }

    fn prepare_import_archive(
        &self,
        archive_path: &Path,
        _archive_is_temporary: bool,
        job_id: &str,
    ) -> Result<ImportArchiveExecutionRequest, DomainError> {
        let source = std::fs::canonicalize(archive_path).map_err(|error| {
            DomainError::InvalidData(format!("Archive is unavailable: {error}"))
        })?;
        let data_root = std::fs::canonicalize(&self.data_root)
            .map_err(|error| DomainError::InternalError(error.to_string()))?;
        if !source.starts_with(&data_root) || !source.is_file() {
            return Err(DomainError::InvalidData(
                "Archive must be an uploaded file inside the app data directory".to_string(),
            ));
        }
        let workspace_root = self.archive_root().join("imports").join(job_id);
        std::fs::create_dir_all(&workspace_root)
            .map_err(|error| DomainError::InternalError(error.to_string()))?;
        let staged_archive = workspace_root.join("archive.zip");
        std::fs::copy(&source, &staged_archive)
            .map_err(|error| DomainError::InternalError(error.to_string()))?;
        Ok(ImportArchiveExecutionRequest {
            data_root: self.data_root.clone(),
            archive_path: staged_archive,
            workspace_root,
        })
    }

    fn prepare_export_archive(
        &self,
        job_id: &str,
        _protected_paths: &[PathBuf],
    ) -> Result<ExportArchiveExecutionRequest, DomainError> {
        let export_root = self.export_root();
        std::fs::create_dir_all(&export_root)
            .map_err(|error| DomainError::InternalError(error.to_string()))?;
        Ok(ExportArchiveExecutionRequest {
            data_root: self.data_root.clone(),
            output_path: export_root.join(format!("export-{job_id}.zip")),
            file_name: "tauritavern-data.zip".to_string(),
        })
    }

    fn prepare_user_backup_archive(
        &self,
        handle: &str,
        include_secrets: bool,
        _protected_paths: &[PathBuf],
    ) -> Result<UserBackupArchiveTarget, DomainError> {
        if handle.is_empty()
            || !handle.chars().all(|character| {
                character.is_ascii_alphanumeric() || character == '-' || character == '_'
            })
        {
            return Err(DomainError::InvalidData(
                "Invalid user backup handle".to_string(),
            ));
        }
        let user_root = self.data_root.join(handle);
        if !user_root.is_dir() {
            return Err(DomainError::NotFound(format!(
                "User data folder not found: {handle}"
            )));
        }
        let output_path = self.export_root().join(format!(
            "user-backup-{}-{}.zip",
            uuid::Uuid::new_v4().simple(),
            handle
        ));
        std::fs::create_dir_all(self.export_root())
            .map_err(|error| DomainError::InternalError(error.to_string()))?;
        Ok(UserBackupArchiveTarget {
            file_name: format!("tauritavern-user-{handle}.zip"),
            request: UserBackupArchiveExecutionRequest {
                user_root,
                output_path,
                include_secrets,
            },
        })
    }

    fn cleanup_directory(&self, path: &Path) {
        if self.ensure_archive_child(path).is_ok() {
            let _ = std::fs::remove_dir_all(path);
        }
    }

    fn cleanup_export(&self, archive_path: &Path) -> Result<(), DomainError> {
        self.ensure_archive_child(archive_path)?;
        std::fs::remove_file(archive_path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                DomainError::NotFound(format!(
                    "Export archive not found: {}",
                    archive_path.display()
                ))
            } else {
                DomainError::InternalError(error.to_string())
            }
        })
    }

    fn save_export(&self, archive_path: &Path, file_name: &str) -> Result<PathBuf, DomainError> {
        self.ensure_archive_child(archive_path)?;
        let target = self.download_target(file_name)?;
        std::fs::copy(archive_path, &target)
            .map_err(|error| DomainError::InternalError(error.to_string()))?;
        Ok(target)
    }

    fn save_user_backup(
        &self,
        archive_path: &str,
        file_name: &str,
    ) -> Result<PathBuf, DomainError> {
        let source = PathBuf::from(archive_path);
        self.ensure_archive_child(&source)?;
        let target = self.download_target(file_name)?;
        std::fs::copy(source, &target)
            .map_err(|error| DomainError::InternalError(error.to_string()))?;
        Ok(target)
    }

    fn cleanup_user_backup(&self, archive_path: &str) -> Result<(), DomainError> {
        let path = PathBuf::from(archive_path);
        self.ensure_archive_child(&path)?;
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(DomainError::InternalError(error.to_string())),
        }
    }
}

struct WebDataRootInitializer;

#[async_trait]
impl DataRootInitializer for WebDataRootInitializer {
    async fn initialize_data_root(&self, data_root: &Path) -> Result<(), DomainError> {
        DataDirectory::new(data_root.to_path_buf())
            .initialize()
            .await
            .map_err(|error| DomainError::InternalError(error.to_string()))
    }
}

struct WebDataChangeReconciler {
    settings: Arc<SettingsService>,
    characters: Arc<CharacterService>,
    chats: Arc<ChatService>,
    groups: Arc<GroupService>,
    group_chats: Arc<GroupChatService>,
    secrets: Arc<SecretService>,
    chat_history: Arc<ChatHistoryCoordinator>,
}

#[async_trait]
impl tt_ports::sync::DataChangeReconciler for WebDataChangeReconciler {
    async fn reconcile(&self, _reason: &str) -> Result<(), DomainError> {
        self.characters.clear_cache().await?;
        self.chats.clear_cache().await?;
        self.groups.clear_cache().await?;
        self.group_chats.clear_cache().await?;
        self.secrets.clear_cache().await?;
        self.settings.reload().await?;
        self.chat_history.invalidate_all_pending().await;
        Ok(())
    }
}

struct WebExternalImportDownloader {
    http_clients: Arc<HttpClientPool>,
}

impl WebExternalImportDownloader {
    fn new(http_clients: Arc<HttpClientPool>) -> Self {
        Self { http_clients }
    }
}

#[async_trait]
impl ExternalImportDownloader for WebExternalImportDownloader {
    async fn fetch_bytes(
        &self,
        url: url::Url,
        limit: Option<DownloadByteLimit>,
    ) -> Result<DownloadedBytes, DomainError> {
        let response = self
            .http_clients
            .client(HttpClientProfile::Download)?
            .get(url)
            .send()
            .await
            .map_err(|error| DomainError::InternalError(error.to_string()))?;
        if !response.status().is_success() {
            return Err(DomainError::InternalError(format!(
                "Upstream responded with HTTP {}",
                response.status()
            )));
        }
        if let Some(limit) = limit
            && response
                .content_length()
                .is_some_and(|length| length > limit.max_bytes as u64)
        {
            return Err(DomainError::InvalidData(format!(
                "{} must be <= {} bytes",
                limit.label, limit.max_bytes
            )));
        }
        let headers = response.headers().clone();
        let bytes = response
            .bytes()
            .await
            .map_err(|error| DomainError::InternalError(error.to_string()))?
            .to_vec();
        if let Some(limit) = limit
            && bytes.len() > limit.max_bytes
        {
            return Err(DomainError::InvalidData(format!(
                "{} must be <= {} bytes",
                limit.label, limit.max_bytes
            )));
        }
        Ok(DownloadedBytes {
            bytes,
            content_type: headers
                .get("content-type")
                .and_then(|value| value.to_str().ok())
                .map(ToOwned::to_owned),
            content_disposition: headers
                .get("content-disposition")
                .and_then(|value| value.to_str().ok())
                .map(ToOwned::to_owned),
        })
    }

    async fn fetch_to_file(&self, url: url::Url, path: &Path) -> Result<(), DomainError> {
        let response = self
            .http_clients
            .client(HttpClientProfile::Download)?
            .get(url)
            .send()
            .await
            .map_err(|error| DomainError::InternalError(error.to_string()))?;
        if !response.status().is_success() {
            return Err(DomainError::InternalError(format!(
                "Upstream responded with HTTP {}",
                response.status()
            )));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|error| DomainError::InternalError(error.to_string()))?;
        tokio::fs::write(path, bytes)
            .await
            .map_err(|error| DomainError::InternalError(error.to_string()))
    }
}
