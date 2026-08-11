use super::{
    DownloadLocation, StorageClass, StorageError, StorageProvider, StorageProviderCapabilities,
    StorageTier,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::multipart::{Form, Part};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct TelegramColdStorageConfig {
    pub provider_id: String,
    pub pool_id: String,
    pub failure_domain: String,
    pub bot_token: String,
    pub chat_ids: Vec<i64>,
    pub api_base_url: String,
    pub state_file: PathBuf,
    pub max_upload_bytes: u64,
    pub request_timeout_seconds: u64,
}

impl std::fmt::Debug for TelegramColdStorageConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TelegramColdStorageConfig")
            .field("provider_id", &self.provider_id)
            .field("pool_id", &self.pool_id)
            .field("failure_domain", &self.failure_domain)
            .field("bot_token", &"<redacted>")
            .field("chat_ids", &self.chat_ids)
            .field("api_base_url", &self.api_base_url)
            .field("state_file", &self.state_file)
            .field("max_upload_bytes", &self.max_upload_bytes)
            .field("request_timeout_seconds", &self.request_timeout_seconds)
            .finish()
    }
}

impl TelegramColdStorageConfig {
    pub fn from_env() -> Result<Self, StorageError> {
        let bot_token = std::env::var("TELEGRAM_BOT_TOKEN").map_err(|_| {
            StorageError::Configuration(
                "TELEGRAM_BOT_TOKEN is required for Telegram COLD".to_owned(),
            )
        })?;
        let chat_ids = std::env::var("TELEGRAM_COLD_CHAT_IDS")
            .map_err(|_| {
                StorageError::Configuration(
                    "TELEGRAM_COLD_CHAT_IDS is required for Telegram COLD".to_owned(),
                )
            })?
            .split(',')
            .filter(|value| !value.trim().is_empty())
            .map(|value| {
                value.trim().parse::<i64>().map_err(|error| {
                    StorageError::Configuration(format!("invalid Telegram chat ID: {error}"))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        if chat_ids.is_empty() {
            return Err(StorageError::Configuration(
                "at least one Telegram COLD chat ID is required".to_owned(),
            ));
        }
        let parse_u64 = |name: &str, default: u64| -> Result<u64, StorageError> {
            std::env::var(name)
                .unwrap_or_else(|_| default.to_string())
                .parse()
                .map_err(|error| {
                    StorageError::Configuration(format!(
                        "{name} must be an unsigned integer: {error}"
                    ))
                })
        };
        let config = Self {
            provider_id: std::env::var("TELEGRAM_COLD_PROVIDER_ID")
                .unwrap_or_else(|_| "telegram-cold".to_owned()),
            pool_id: std::env::var("TELEGRAM_COLD_POOL_ID")
                .unwrap_or_else(|_| "telegram-cold".to_owned()),
            failure_domain: std::env::var("TELEGRAM_COLD_FAILURE_DOMAIN")
                .unwrap_or_else(|_| "telegram".to_owned()),
            bot_token,
            chat_ids,
            api_base_url: std::env::var("TELEGRAM_BOT_API_BASE_URL")
                .unwrap_or_else(|_| "https://api.telegram.org".to_owned())
                .trim_end_matches('/')
                .to_owned(),
            state_file: std::env::var_os("TELEGRAM_COLD_STATE_FILE")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("telegram-cold-state.json")),
            max_upload_bytes: parse_u64("TELEGRAM_COLD_MAX_UPLOAD_BYTES", 50 * 1024 * 1024)?,
            request_timeout_seconds: parse_u64("TELEGRAM_COLD_REQUEST_TIMEOUT_SECONDS", 120)?,
        };
        if config.provider_id.trim().is_empty()
            || config.pool_id.trim().is_empty()
            || config.failure_domain.trim().is_empty()
            || config.bot_token.trim().is_empty()
            || config.max_upload_bytes == 0
            || config.request_timeout_seconds == 0
        {
            return Err(StorageError::Configuration(
                "Telegram COLD configuration contains an empty or invalid value".to_owned(),
            ));
        }
        Ok(config)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TelegramObjectRef {
    chat_id: i64,
    message_id: i64,
    file_id: String,
    file_unique_id: String,
    size: u64,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct TelegramState {
    objects: HashMap<String, TelegramObjectRef>,
}

#[derive(Clone)]
pub struct TelegramColdStorageProvider {
    config: Arc<TelegramColdStorageConfig>,
    client: reqwest::Client,
    state: Arc<Mutex<TelegramState>>,
    loaded: Arc<Mutex<bool>>,
    next_chat: Arc<Mutex<usize>>,
}

impl std::fmt::Debug for TelegramColdStorageProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TelegramColdStorageProvider")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl TelegramColdStorageProvider {
    pub fn new(config: TelegramColdStorageConfig) -> Result<Self, StorageError> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(
                config.request_timeout_seconds,
            ))
            .build()
            .map_err(|error| {
                StorageError::Configuration(format!("could not create Telegram client: {error}"))
            })?;
        Ok(Self {
            config: Arc::new(config),
            client,
            state: Arc::new(Mutex::new(TelegramState::default())),
            loaded: Arc::new(Mutex::new(false)),
            next_chat: Arc::new(Mutex::new(0)),
        })
    }

    pub fn from_env() -> Result<Self, StorageError> {
        Self::new(TelegramColdStorageConfig::from_env()?)
    }

    pub fn config(&self) -> &TelegramColdStorageConfig {
        &self.config
    }

    async fn load_state(&self) -> Result<(), StorageError> {
        let path = &self.config.state_file;
        let state = match tokio::fs::read(path).await {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => TelegramState::default(),
            Err(error) => return Err(StorageError::Io(error)),
        };
        *self.state.lock().await = state;
        Ok(())
    }

    async fn ensure_state_loaded(&self) -> Result<(), StorageError> {
        let mut loaded = self.loaded.lock().await;
        if !*loaded {
            self.load_state().await?;
            *loaded = true;
        }
        Ok(())
    }

    async fn save_state(&self) -> Result<(), StorageError> {
        let path = &self.config.state_file;
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            tokio::fs::create_dir_all(parent).await?;
        }
        let state = self.state.lock().await;
        let temporary = path.with_extension("json.part");
        tokio::fs::write(&temporary, serde_json::to_vec_pretty(&*state)?).await?;
        tokio::fs::rename(temporary, path).await?;
        Ok(())
    }

    fn endpoint(&self, method: &str) -> String {
        format!(
            "{}/bot{}/{}",
            self.config.api_base_url, self.config.bot_token, method
        )
    }
    fn file_endpoint(&self, file_path: &str) -> String {
        format!(
            "{}/file/bot{}/{}",
            self.config.api_base_url,
            self.config.bot_token,
            file_path.trim_start_matches('/')
        )
    }

    async fn call<T: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        request: reqwest::RequestBuilder,
    ) -> Result<T, StorageError> {
        let response = request.send().await.map_err(|error| {
            StorageError::NetworkUnavailable(format!("Telegram {method} request failed: {error}"))
        })?;
        let status = response.status();
        let body = response.bytes().await.map_err(|error| {
            StorageError::NetworkUnavailable(format!("Telegram {method} response failed: {error}"))
        })?;
        let envelope: TelegramResponse<T> = serde_json::from_slice(&body).map_err(|error| {
            StorageError::Provider(format!("Telegram {method} returned invalid JSON: {error}"))
        })?;
        if !status.is_success() || !envelope.ok {
            return Err(StorageError::Provider(format!(
                "Telegram {method} failed with HTTP {}: {}",
                status.as_u16(),
                envelope
                    .description
                    .unwrap_or_else(|| "unspecified API error".to_owned())
            )));
        }
        envelope
            .result
            .ok_or_else(|| StorageError::Provider(format!("Telegram {method} returned no result")))
    }

    async fn target_chat(&self) -> i64 {
        let mut next = self.next_chat.lock().await;
        let chat = self.config.chat_ids[*next % self.config.chat_ids.len()];
        *next = (*next + 1) % self.config.chat_ids.len();
        chat
    }

    async fn upload(&self, hash: &str, bytes: &[u8], extension: &str) -> Result<(), StorageError> {
        self.ensure_state_loaded().await?;
        if bytes.len() as u64 > self.config.max_upload_bytes {
            return Err(StorageError::Configuration(format!(
                "Telegram COLD object exceeds configured upload limit of {} bytes",
                self.config.max_upload_bytes
            )));
        }
        super::validate_hash(hash)?;
        let chat_id = self.target_chat().await;
        let document = Part::bytes(bytes.to_vec()).file_name(format!("{hash}{extension}"));
        let form = Form::new()
            .text("chat_id", chat_id.to_string())
            .text("caption", format!("launcher-object:{hash}"))
            .part("document", document);
        let message: TelegramMessage = self
            .call(
                "sendDocument",
                self.client
                    .post(self.endpoint("sendDocument"))
                    .multipart(form),
            )
            .await?;
        let document = message.document.ok_or_else(|| {
            StorageError::Provider("Telegram did not return a document reference".to_owned())
        })?;
        self.state.lock().await.objects.insert(
            hash.to_owned(),
            TelegramObjectRef {
                chat_id,
                message_id: message.message_id,
                file_id: document.file_id,
                file_unique_id: document.file_unique_id,
                size: bytes.len() as u64,
                updated_at: Utc::now(),
            },
        );
        self.save_state().await
    }

    async fn read(&self, hash: &str) -> Result<Vec<u8>, StorageError> {
        self.ensure_state_loaded().await?;
        super::validate_hash(hash)?;
        let reference = self
            .state
            .lock()
            .await
            .objects
            .get(hash)
            .cloned()
            .ok_or_else(|| {
                StorageError::Unavailable(format!(
                    "Telegram object {hash} is not enrolled in the worker state"
                ))
            })?;
        let file: TelegramFile = self
            .call(
                "getFile",
                self.client
                    .post(self.endpoint("getFile"))
                    .form(&[("file_id", reference.file_id.as_str())]),
            )
            .await?;
        let path = file.file_path.ok_or_else(|| {
            StorageError::Provider("Telegram getFile returned no file path".to_owned())
        })?;
        let response = self
            .client
            .get(self.file_endpoint(&path))
            .send()
            .await
            .map_err(|error| {
                StorageError::NetworkUnavailable(format!("Telegram file download failed: {error}"))
            })?;
        if !response.status().is_success() {
            return Err(StorageError::Provider(format!(
                "Telegram file download returned HTTP {}",
                response.status().as_u16()
            )));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|error| {
                StorageError::NetworkUnavailable(format!("Telegram file read failed: {error}"))
            })?
            .to_vec();
        if bytes.len() as u64 != reference.size {
            return Err(StorageError::Provider(
                "Telegram file size verification failed".to_owned(),
            ));
        }
        Ok(bytes)
    }

    async fn delete(&self, hash: &str) -> Result<(), StorageError> {
        self.ensure_state_loaded().await?;
        let Some(reference) = self.state.lock().await.objects.remove(hash) else {
            return Ok(());
        };
        let _: bool = self
            .call(
                "deleteMessage",
                self.client.post(self.endpoint("deleteMessage")).form(&[
                    ("chat_id", reference.chat_id.to_string()),
                    ("message_id", reference.message_id.to_string()),
                ]),
            )
            .await?;
        self.save_state().await
    }
}

#[async_trait]
impl StorageProvider for TelegramColdStorageProvider {
    fn provider_id(&self) -> &str {
        &self.config.provider_id
    }
    fn tier(&self) -> StorageTier {
        StorageClass::Cold
    }
    fn pool_id(&self) -> &str {
        &self.config.pool_id
    }
    fn provider_type(&self) -> &str {
        "telegram"
    }
    fn failure_domain(&self) -> &str {
        &self.config.failure_domain
    }
    fn capabilities(&self) -> StorageProviderCapabilities {
        StorageProviderCapabilities::cold_server_only()
    }

    async fn put_encoded(&self, encoded_hash: &str, bytes: &[u8]) -> Result<(), StorageError> {
        super::verify_encoded_bytes(encoded_hash, bytes)?;
        self.upload(encoded_hash, bytes, ".chunk").await
    }
    async fn read_encoded(&self, encoded_hash: &str) -> Result<Vec<u8>, StorageError> {
        let bytes = self.read(encoded_hash).await?;
        super::verify_encoded_bytes(encoded_hash, &bytes)?;
        Ok(bytes)
    }
    async fn delete_encoded(&self, encoded_hash: &str) -> Result<(), StorageError> {
        self.delete(encoded_hash).await
    }
    async fn download_location(
        &self,
        _encoded_hash: &str,
    ) -> Result<DownloadLocation, StorageError> {
        Err(StorageError::Provider(
            "Telegram COLD is server-side only".to_owned(),
        ))
    }
    async fn put_pack(&self, pack_hash: &str, bytes: &[u8]) -> Result<(), StorageError> {
        super::verify_pack_bytes(pack_hash, bytes)?;
        self.upload(pack_hash, bytes, ".pack").await
    }
    async fn read_pack(&self, pack_hash: &str) -> Result<Vec<u8>, StorageError> {
        let bytes = self.read(pack_hash).await?;
        super::verify_pack_bytes(pack_hash, &bytes)?;
        Ok(bytes)
    }
    async fn delete_pack(&self, pack_hash: &str) -> Result<(), StorageError> {
        self.delete(pack_hash).await
    }
    async fn download_pack_location(
        &self,
        _pack_hash: &str,
    ) -> Result<DownloadLocation, StorageError> {
        Err(StorageError::Provider(
            "Telegram COLD is server-side only".to_owned(),
        ))
    }

    async fn health_check(&self) -> Result<(), StorageError> {
        self.ensure_state_loaded().await?;
        let _: TelegramUser = self
            .call("getMe", self.client.get(self.endpoint("getMe")))
            .await?;
        for chat_id in &self.config.chat_ids {
            let _: TelegramChat = self
                .call(
                    "getChat",
                    self.client
                        .post(self.endpoint("getChat"))
                        .form(&[("chat_id", chat_id.to_string())]),
                )
                .await?;
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct TelegramResponse<T> {
    ok: bool,
    result: Option<T>,
    description: Option<String>,
}
#[derive(Debug, Deserialize)]
struct TelegramUser {
    #[allow(dead_code)]
    id: i64,
}
#[derive(Debug, Deserialize)]
struct TelegramChat {
    #[allow(dead_code)]
    id: i64,
}
#[derive(Debug, Deserialize)]
struct TelegramMessage {
    message_id: i64,
    document: Option<TelegramDocument>,
}
#[derive(Debug, Deserialize)]
struct TelegramDocument {
    file_id: String,
    file_unique_id: String,
}
#[derive(Debug, Deserialize)]
struct TelegramFile {
    file_path: Option<String>,
}
