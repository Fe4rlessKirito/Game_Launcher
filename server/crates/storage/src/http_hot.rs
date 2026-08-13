use async_trait::async_trait;
use bytes::Bytes;
use futures_util::{StreamExt, stream};
use reqwest::multipart::{Form, Part};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::sync::Semaphore;

use crate::{
    DownloadLocation, StorageByteStream, StorageError, StorageProvider,
    StorageProviderCapabilities, StorageTier, validate_hash, validate_pack_hash,
    verify_encoded_bytes, verify_pack_bytes,
};

const DEFAULT_FILEMIRAGE_BASE_URL: &str = "https://filemirage.com";
const DEFAULT_FILEMIRAGE_CHUNK_BYTES: usize = 99 * 1024 * 1024;
const DEFAULT_BUZZHEAVIER_UPLOAD_URL: &str = "https://w.buzzheavier.com";
const DEFAULT_BUZZHEAVIER_DOWNLOAD_URL: &str = "https://buzzheavier.com";
const DEFAULT_MAX_OBJECT_BYTES: u64 = 50 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HttpHotKind {
    FileMirage,
    Buzzheavier,
}

impl HttpHotKind {
    fn provider_type(self) -> &'static str {
        match self {
            Self::FileMirage => "filemirage",
            Self::Buzzheavier => "buzzheavier",
        }
    }
}

#[derive(Debug, Clone)]
pub struct FileMirageStorageConfig {
    pub provider_id: String,
    pub base_url: String,
    pub upload_server_url: Option<String>,
    pub api_token: Option<String>,
    pub state_file: PathBuf,
    pub upload_chunk_bytes: usize,
    pub request_timeout: Duration,
    pub max_concurrent_requests: usize,
    pub delete_proven: bool,
}

impl FileMirageStorageConfig {
    pub fn from_env(state_root: &Path) -> Result<Self, StorageError> {
        Ok(Self {
            provider_id: env_string("LAUNCHER_FILEMIRAGE_PROVIDER_ID", "filemirage"),
            base_url: env_string("LAUNCHER_FILEMIRAGE_BASE_URL", DEFAULT_FILEMIRAGE_BASE_URL),
            upload_server_url: env_optional("LAUNCHER_FILEMIRAGE_UPLOAD_SERVER_URL"),
            api_token: env_optional("LAUNCHER_FILEMIRAGE_API_TOKEN"),
            state_file: env_path(
                "LAUNCHER_FILEMIRAGE_STATE_FILE",
                state_root.join("filemirage-state.json"),
            ),
            upload_chunk_bytes: env_usize(
                "LAUNCHER_FILEMIRAGE_UPLOAD_CHUNK_BYTES",
                DEFAULT_FILEMIRAGE_CHUNK_BYTES,
            )?,
            request_timeout: Duration::from_secs(env_u64(
                "LAUNCHER_FILEMIRAGE_REQUEST_TIMEOUT_SECONDS",
                300,
            )?),
            max_concurrent_requests: env_usize("LAUNCHER_FILEMIRAGE_MAX_CONCURRENT_REQUESTS", 4)?,
            delete_proven: env_bool("LAUNCHER_FILEMIRAGE_DELETE_PROVEN", false),
        })
    }

    fn validate(&self) -> Result<(), StorageError> {
        validate_common_config(
            &self.provider_id,
            &self.base_url,
            self.upload_chunk_bytes,
            self.max_concurrent_requests,
        )?;
        if self.upload_chunk_bytes == 0 {
            return Err(StorageError::Configuration(
                "FileMirage upload chunk size must be positive".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct BuzzheavierStorageConfig {
    pub provider_id: String,
    pub upload_base_url: String,
    pub download_base_url: String,
    pub account_id: Option<String>,
    pub state_file: PathBuf,
    pub request_timeout: Duration,
    pub max_concurrent_requests: usize,
    pub direct_download_proven: bool,
    pub range_requests_proven: bool,
    pub delete_proven: bool,
}

impl BuzzheavierStorageConfig {
    pub fn from_env(state_root: &Path) -> Result<Self, StorageError> {
        Ok(Self {
            provider_id: env_string("LAUNCHER_BUZZHEAVIER_PROVIDER_ID", "buzzheavier"),
            upload_base_url: env_string(
                "LAUNCHER_BUZZHEAVIER_UPLOAD_BASE_URL",
                DEFAULT_BUZZHEAVIER_UPLOAD_URL,
            ),
            download_base_url: env_string(
                "LAUNCHER_BUZZHEAVIER_DOWNLOAD_BASE_URL",
                DEFAULT_BUZZHEAVIER_DOWNLOAD_URL,
            ),
            account_id: env_optional("LAUNCHER_BUZZHEAVIER_ACCOUNT_ID"),
            state_file: env_path(
                "LAUNCHER_BUZZHEAVIER_STATE_FILE",
                state_root.join("buzzheavier-state.json"),
            ),
            request_timeout: Duration::from_secs(env_u64(
                "LAUNCHER_BUZZHEAVIER_REQUEST_TIMEOUT_SECONDS",
                300,
            )?),
            max_concurrent_requests: env_usize("LAUNCHER_BUZZHEAVIER_MAX_CONCURRENT_REQUESTS", 2)?,
            direct_download_proven: env_bool("LAUNCHER_BUZZHEAVIER_DIRECT_DOWNLOAD_PROVEN", false),
            range_requests_proven: env_bool("LAUNCHER_BUZZHEAVIER_RANGE_REQUESTS_PROVEN", false),
            delete_proven: env_bool("LAUNCHER_BUZZHEAVIER_DELETE_PROVEN", false),
        })
    }

    fn validate(&self) -> Result<(), StorageError> {
        validate_common_config(
            &self.provider_id,
            &self.upload_base_url,
            0,
            self.max_concurrent_requests,
        )?;
        if self.direct_download_proven && self.download_base_url.trim().is_empty() {
            return Err(StorageError::Configuration(
                "Buzzheavier download base URL is required when direct download is enabled"
                    .to_owned(),
            ));
        }
        if self.range_requests_proven && !self.direct_download_proven {
            return Err(StorageError::Configuration(
                "Buzzheavier range support cannot be enabled before direct download support"
                    .to_owned(),
            ));
        }
        if self.delete_proven && self.account_id.is_none() {
            return Err(StorageError::Configuration(
                "Buzzheavier delete support requires an account ID".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RemoteReference {
    url: String,
    remote_id: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RemoteState {
    objects: HashMap<String, RemoteReference>,
    packs: HashMap<String, RemoteReference>,
}

#[derive(Clone)]
pub struct FileMirageStorage {
    inner: HttpHotStorage,
}

impl std::fmt::Debug for FileMirageStorage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.inner.fmt(formatter)
    }
}

impl FileMirageStorage {
    pub fn new(config: FileMirageStorageConfig) -> Result<Self, StorageError> {
        config.validate()?;
        Ok(Self {
            inner: HttpHotStorage::new(
                HttpHotKind::FileMirage,
                config.base_url,
                config.upload_server_url,
                None,
                config.api_token,
                config.state_file,
                config.upload_chunk_bytes,
                config.request_timeout,
                config.max_concurrent_requests,
                true,
                true,
                config.delete_proven,
                false,
                false,
                config.provider_id,
            )?,
        })
    }
}

#[derive(Clone)]
pub struct BuzzheavierStorage {
    inner: HttpHotStorage,
}

impl std::fmt::Debug for BuzzheavierStorage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.inner.fmt(formatter)
    }
}

impl BuzzheavierStorage {
    pub fn new(config: BuzzheavierStorageConfig) -> Result<Self, StorageError> {
        config.validate()?;
        Ok(Self {
            inner: HttpHotStorage::new(
                HttpHotKind::Buzzheavier,
                config.upload_base_url,
                None,
                Some(config.download_base_url),
                config.account_id,
                config.state_file,
                0,
                config.request_timeout,
                config.max_concurrent_requests,
                config.direct_download_proven,
                config.range_requests_proven,
                config.delete_proven,
                false,
                false,
                config.provider_id,
            )?,
        })
    }
}

#[derive(Clone)]
struct HttpHotStorage {
    kind: HttpHotKind,
    client: reqwest::Client,
    base_url: String,
    upload_server_url: Option<String>,
    download_base_url: Option<String>,
    api_token: Option<String>,
    state_file: PathBuf,
    upload_chunk_bytes: usize,
    request_slots: Arc<Semaphore>,
    state: Arc<Mutex<RemoteState>>,
    state_write: Arc<tokio::sync::Mutex<()>>,
    direct_download_proven: bool,
    range_requests_proven: bool,
    delete_proven: bool,
    stable_urls: bool,
    expiring_urls: bool,
    provider_id: String,
}

impl std::fmt::Debug for HttpHotStorage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HttpHotStorage")
            .field("kind", &self.kind)
            .field("base_url", &self.base_url)
            .field("upload_server_url", &self.upload_server_url)
            .field("download_base_url", &self.download_base_url)
            .field("api_token", &self.api_token.as_ref().map(|_| "<redacted>"))
            .field("state_file", &self.state_file)
            .field("upload_chunk_bytes", &self.upload_chunk_bytes)
            .field("direct_download_proven", &self.direct_download_proven)
            .field("range_requests_proven", &self.range_requests_proven)
            .field("delete_proven", &self.delete_proven)
            .field("provider_id", &self.provider_id)
            .finish()
    }
}

impl HttpHotStorage {
    #[allow(clippy::too_many_arguments)]
    fn new(
        kind: HttpHotKind,
        base_url: String,
        upload_server_url: Option<String>,
        download_base_url: Option<String>,
        api_token: Option<String>,
        state_file: PathBuf,
        upload_chunk_bytes: usize,
        request_timeout: Duration,
        max_concurrent_requests: usize,
        direct_download_proven: bool,
        range_requests_proven: bool,
        delete_proven: bool,
        stable_urls: bool,
        expiring_urls: bool,
        provider_id: String,
    ) -> Result<Self, StorageError> {
        let client = reqwest::Client::builder()
            .user_agent("Vaultnode-Launcher/1.0")
            .timeout(request_timeout)
            .build()
            .map_err(|error| StorageError::Configuration(error.to_string()))?;
        let state = match std::fs::read(&state_file) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| {
                StorageError::Configuration(format!(
                    "{} state file is invalid: {error}",
                    kind.provider_type()
                ))
            })?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => RemoteState::default(),
            Err(error) => return Err(StorageError::Io(error)),
        };
        Ok(Self {
            kind,
            client,
            base_url: base_url.trim_end_matches('/').to_owned(),
            upload_server_url: upload_server_url
                .map(|value| value.trim_end_matches('/').to_owned()),
            download_base_url: download_base_url
                .map(|value| value.trim_end_matches('/').to_owned()),
            api_token,
            state_file,
            upload_chunk_bytes,
            request_slots: Arc::new(Semaphore::new(max_concurrent_requests)),
            state: Arc::new(Mutex::new(state)),
            state_write: Arc::new(tokio::sync::Mutex::new(())),
            direct_download_proven,
            range_requests_proven,
            delete_proven,
            stable_urls,
            expiring_urls,
            provider_id,
        })
    }

    fn auth(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match self.api_token.as_ref() {
            Some(token) => request.bearer_auth(token),
            None => request,
        }
    }

    async fn acquire_slot(&self) -> Result<tokio::sync::OwnedSemaphorePermit, StorageError> {
        self.request_slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| StorageError::RateLimiterClosed)
    }

    async fn health(&self) -> Result<(), StorageError> {
        let _slot = self.acquire_slot().await?;
        let url = match self.kind {
            HttpHotKind::FileMirage => format!("{}/api/servers", self.base_url),
            HttpHotKind::Buzzheavier => format!(
                "{}/api/locations",
                self.download_base_url.as_deref().unwrap_or(&self.base_url)
            ),
        };
        let response = self
            .auth(self.client.get(url))
            .send()
            .await
            .map_err(|error| {
                StorageError::NetworkUnavailable(format!(
                    "{} health request failed: {error}",
                    self.kind.provider_type()
                ))
            })?;
        if !response.status().is_success() {
            return Err(StorageError::Provider(format!(
                "{} health request returned HTTP {}",
                self.kind.provider_type(),
                response.status().as_u16()
            )));
        }
        Ok(())
    }

    fn capabilities_for(&self) -> StorageProviderCapabilities {
        StorageProviderCapabilities {
            upload: true,
            delete: self.delete_proven,
            direct_download: self.direct_download_proven,
            range_requests: self.range_requests_proven,
            stable_urls: self.stable_urls,
            expiring_urls: self.expiring_urls,
            url_refresh: false,
            requires_authentication: self.api_token.is_some(),
            max_object_size_bytes: Some(match self.kind {
                HttpHotKind::FileMirage => DEFAULT_MAX_OBJECT_BYTES,
                HttpHotKind::Buzzheavier => u64::MAX,
            }),
            preferred_pack_size_bytes: Some(512 * 1024 * 1024),
            recommended_concurrency: self.request_slots.available_permits() as u32,
        }
    }

    fn state_reference(&self, hash: &str, pack: bool) -> Result<RemoteReference, StorageError> {
        let state = self.state.lock().map_err(|_| {
            StorageError::Provider(format!("{} state lock poisoned", self.kind.provider_type()))
        })?;
        let reference = if pack {
            state.packs.get(hash)
        } else {
            state.objects.get(hash)
        };
        reference.cloned().ok_or_else(|| {
            StorageError::Unavailable(format!(
                "{} object {hash} is not enrolled in provider state",
                self.kind.provider_type()
            ))
        })
    }

    async fn remember(
        &self,
        hash: &str,
        url: String,
        remote_id: Option<String>,
        pack: bool,
    ) -> Result<(), StorageError> {
        let _write_guard = self.state_write.lock().await;
        let snapshot = {
            let mut state = self.state.lock().map_err(|_| {
                StorageError::Provider(format!("{} state lock poisoned", self.kind.provider_type()))
            })?;
            let reference = RemoteReference { url, remote_id };
            if pack {
                state.packs.insert(hash.to_owned(), reference);
            } else {
                state.objects.insert(hash.to_owned(), reference);
            }
            serde_json::to_vec_pretty(&*state)?
        };
        if let Some(parent) = self.state_file.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let temporary = self.state_file.with_file_name(format!(
            "{}.{}.part",
            self.state_file
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("state.json"),
            unique_suffix()
        ));
        tokio::fs::write(&temporary, snapshot).await?;
        if let Err(error) = tokio::fs::rename(&temporary, &self.state_file).await {
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(error.into());
        }
        Ok(())
    }

    async fn upload_filemirage_bytes(
        &self,
        hash: &str,
        bytes: &[u8],
        filename: &str,
        pack: bool,
    ) -> Result<(), StorageError> {
        let server = self.filemirage_server().await?;
        let upload_id = format!("vaultnode-{}", unique_suffix());
        let total_chunks = bytes.len().div_ceil(self.upload_chunk_bytes).max(1);
        let mut remote_url = None;
        for (chunk_number, chunk) in bytes.chunks(self.upload_chunk_bytes).enumerate() {
            let form = Form::new()
                .text("filename", filename.to_owned())
                .text("upload_id", upload_id.clone())
                .text("chunk_number", chunk_number.to_string())
                .text("total_chunks", total_chunks.to_string())
                .part(
                    "file",
                    Part::bytes(chunk.to_vec()).file_name(filename.to_owned()),
                );
            let response = self
                .auth(
                    self.client
                        .post(format!("{server}/upload.php"))
                        .multipart(form),
                )
                .send()
                .await
                .map_err(|error| {
                    StorageError::NetworkUnavailable(format!("FileMirage upload failed: {error}"))
                })?;
            let status = response.status();
            let body = response.bytes().await.map_err(|error| {
                StorageError::NetworkUnavailable(format!(
                    "FileMirage upload response failed: {error}"
                ))
            })?;
            if !status.is_success() {
                return Err(StorageError::Provider(format!(
                    "FileMirage upload returned HTTP {}",
                    status.as_u16()
                )));
            }
            remote_url = extract_url(&body, &server).or(remote_url);
        }
        let page_url = remote_url.ok_or_else(|| {
            StorageError::Provider("FileMirage upload returned no direct URL".to_owned())
        })?;
        let url = self.resolve_filemirage_url(&page_url).await?;
        self.remember(hash, url, None, pack).await
    }

    async fn upload_filemirage_file(
        &self,
        hash: &str,
        path: &Path,
        filename: &str,
        pack: bool,
    ) -> Result<(), StorageError> {
        let server = self.filemirage_server().await?;
        let size = tokio::fs::metadata(path).await?.len();
        let total_chunks = usize::try_from(size)
            .map_err(|_| StorageError::Configuration("FileMirage object is too large".to_owned()))?
            .div_ceil(self.upload_chunk_bytes)
            .max(1);
        let upload_id = format!("vaultnode-{}", unique_suffix());
        let mut file = tokio::fs::File::open(path).await?;
        let mut remote_url = None;
        for chunk_number in 0..total_chunks {
            let remaining =
                size.saturating_sub(chunk_number as u64 * self.upload_chunk_bytes as u64);
            let chunk_size = usize::try_from(remaining.min(self.upload_chunk_bytes as u64))
                .map_err(|_| {
                    StorageError::Configuration("FileMirage chunk is too large".to_owned())
                })?;
            let mut chunk = vec![0_u8; chunk_size];
            file.read_exact(&mut chunk).await?;
            let form = Form::new()
                .text("filename", filename.to_owned())
                .text("upload_id", upload_id.clone())
                .text("chunk_number", chunk_number.to_string())
                .text("total_chunks", total_chunks.to_string())
                .part("file", Part::bytes(chunk).file_name(filename.to_owned()));
            let response = self
                .auth(
                    self.client
                        .post(format!("{server}/upload.php"))
                        .multipart(form),
                )
                .send()
                .await
                .map_err(|error| {
                    StorageError::NetworkUnavailable(format!("FileMirage upload failed: {error}"))
                })?;
            let status = response.status();
            let body = response.bytes().await.map_err(|error| {
                StorageError::NetworkUnavailable(format!(
                    "FileMirage upload response failed: {error}"
                ))
            })?;
            if !status.is_success() {
                return Err(StorageError::Provider(format!(
                    "FileMirage upload returned HTTP {}",
                    status.as_u16()
                )));
            }
            remote_url = extract_url(&body, &server).or(remote_url);
        }
        let page_url = remote_url.ok_or_else(|| {
            StorageError::Provider("FileMirage upload returned no direct URL".to_owned())
        })?;
        let url = self.resolve_filemirage_url(&page_url).await?;
        self.remember(hash, url, None, pack).await
    }

    async fn filemirage_server(&self) -> Result<String, StorageError> {
        if let Some(server) = &self.upload_server_url {
            return Ok(server.clone());
        }
        let _slot = self.acquire_slot().await?;
        let response = self
            .auth(self.client.get(format!("{}/api/servers", self.base_url)))
            .send()
            .await
            .map_err(|error| {
                StorageError::NetworkUnavailable(format!(
                    "FileMirage server lookup failed: {error}"
                ))
            })?;
        if !response.status().is_success() {
            return Err(StorageError::Provider(format!(
                "FileMirage server lookup returned HTTP {}",
                response.status().as_u16()
            )));
        }
        let body: serde_json::Value = response.json().await.map_err(|error| {
            StorageError::Provider(format!(
                "FileMirage server lookup returned invalid JSON: {error}"
            ))
        })?;
        body.pointer("/data/server")
            .and_then(serde_json::Value::as_str)
            .map(|server| server.trim_end_matches('/').to_owned())
            .ok_or_else(|| {
                StorageError::Provider("FileMirage returned no upload server".to_owned())
            })
    }

    async fn resolve_filemirage_url(&self, page_url: &str) -> Result<String, StorageError> {
        if !matches!(self.kind, HttpHotKind::FileMirage) || page_url.contains("/file/direct/") {
            return Ok(page_url.to_owned());
        }
        let _slot = self.acquire_slot().await?;
        let response = self
            .auth(self.client.get(page_url))
            .send()
            .await
            .map_err(|error| {
                StorageError::NetworkUnavailable(format!(
                    "FileMirage share page lookup failed: {error}"
                ))
            })?;
        if !response.status().is_success() {
            return Err(StorageError::Provider(format!(
                "FileMirage share page lookup returned HTTP {}",
                response.status().as_u16()
            )));
        }
        let html = response.text().await.map_err(|error| {
            StorageError::NetworkUnavailable(format!("FileMirage share page body failed: {error}"))
        })?;
        extract_filemirage_direct_url(&html).ok_or_else(|| {
            StorageError::Provider(
                "FileMirage share page returned no embedded direct download URL".to_owned(),
            )
        })
    }

    async fn upload_buzzheavier_bytes(
        &self,
        hash: &str,
        bytes: &[u8],
        filename: &str,
        pack: bool,
    ) -> Result<(), StorageError> {
        let _slot = self.acquire_slot().await?;
        let response = self
            .auth(
                self.client
                    .put(format!("{}/{}", self.base_url, filename))
                    .body(bytes.to_vec()),
            )
            .send()
            .await
            .map_err(|error| {
                StorageError::NetworkUnavailable(format!("Buzzheavier upload failed: {error}"))
            })?;
        self.record_buzz_response(response, hash, pack).await
    }

    async fn upload_buzzheavier_file(
        &self,
        hash: &str,
        path: &Path,
        pack: bool,
    ) -> Result<(), StorageError> {
        let _slot = self.acquire_slot().await?;
        let size = tokio::fs::metadata(path).await?.len();
        let file = tokio::fs::File::open(path).await?;
        let body = stream::unfold(Some((file, vec![0_u8; 1024 * 1024])), |state| async move {
            let (mut file, mut buffer) = state?;
            match file.read(&mut buffer).await {
                Ok(0) => None,
                Ok(read) => Some((
                    Ok::<Bytes, std::io::Error>(Bytes::copy_from_slice(&buffer[..read])),
                    Some((file, buffer)),
                )),
                Err(error) => Some((Err(error), None)),
            }
        });
        let response = self
            .auth(
                self.client
                    .put(format!("{}/{}", self.base_url, hash))
                    .header(reqwest::header::CONTENT_LENGTH, size)
                    .body(reqwest::Body::wrap_stream(body)),
            )
            .send()
            .await
            .map_err(|error| {
                StorageError::NetworkUnavailable(format!("Buzzheavier upload failed: {error}"))
            })?;
        self.record_buzz_response(response, hash, pack).await
    }

    async fn record_buzz_response(
        &self,
        response: reqwest::Response,
        hash: &str,
        pack: bool,
    ) -> Result<(), StorageError> {
        let status = response.status();
        let body: serde_json::Value = response.json().await.map_err(|error| {
            StorageError::Provider(format!("Buzzheavier upload returned invalid JSON: {error}"))
        })?;
        if !status.is_success() {
            return Err(StorageError::Provider(format!(
                "Buzzheavier upload returned HTTP {}",
                status.as_u16()
            )));
        }
        let remote_id = body
            .pointer("/data/id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                StorageError::Provider("Buzzheavier upload returned no file ID".to_owned())
            })?;
        let url = self.buzz_url(remote_id)?;
        self.remember(hash, url, Some(remote_id.to_owned()), pack)
            .await
    }

    fn buzz_url(&self, remote_id: &str) -> Result<String, StorageError> {
        let base = self.download_base_url.as_ref().ok_or_else(|| {
            StorageError::Configuration("Buzzheavier download base URL is missing".to_owned())
        })?;
        Ok(format!("{base}/{remote_id}/download"))
    }

    async fn read_remote(&self, hash: &str, pack: bool) -> Result<Vec<u8>, StorageError> {
        if !self.direct_download_proven {
            return Err(StorageError::Unavailable(format!(
                "{} direct download is not proven; provider is upload-only",
                self.kind.provider_type()
            )));
        }
        let reference = self.state_reference(hash, pack)?;
        let url = self.resolve_filemirage_url(&reference.url).await?;
        let _slot = self.acquire_slot().await?;
        let response = self
            .auth(self.client.get(url))
            .send()
            .await
            .map_err(|error| {
                StorageError::NetworkUnavailable(format!(
                    "{} download failed: {error}",
                    self.kind.provider_type()
                ))
            })?;
        if !response.status().is_success() {
            return Err(StorageError::Provider(format!(
                "{} download returned HTTP {}",
                self.kind.provider_type(),
                response.status().as_u16()
            )));
        }
        response
            .bytes()
            .await
            .map(|bytes| bytes.to_vec())
            .map_err(|error| {
                StorageError::NetworkUnavailable(format!(
                    "{} download body failed: {error}",
                    self.kind.provider_type()
                ))
            })
    }

    async fn delete_remote(&self, hash: &str, pack: bool) -> Result<(), StorageError> {
        if !self.delete_proven {
            return Err(StorageError::Provider(format!(
                "{} delete is not proven or configured",
                self.kind.provider_type()
            )));
        }
        let reference = self.state_reference(hash, pack)?;
        let remote_id = reference.remote_id.ok_or_else(|| {
            StorageError::Provider(format!(
                "{} object has no remote ID",
                self.kind.provider_type()
            ))
        })?;
        let _slot = self.acquire_slot().await?;
        let response = self
            .auth(self.client.delete(format!(
                "{}/api/fs/{remote_id}",
                self.download_base_url.as_deref().unwrap_or(&self.base_url)
            )))
            .send()
            .await
            .map_err(|error| {
                StorageError::NetworkUnavailable(format!(
                    "{} delete failed: {error}",
                    self.kind.provider_type()
                ))
            })?;
        if !response.status().is_success() {
            return Err(StorageError::Provider(format!(
                "{} delete returned HTTP {}",
                self.kind.provider_type(),
                response.status().as_u16()
            )));
        }
        Ok(())
    }

    async fn download_location_for(
        &self,
        hash: &str,
        pack: bool,
    ) -> Result<DownloadLocation, StorageError> {
        if !self.direct_download_proven {
            return Err(StorageError::Unavailable(format!(
                "{} direct download is not proven",
                self.kind.provider_type()
            )));
        }
        let reference = self.state_reference(hash, pack)?;
        let url = self.resolve_filemirage_url(&reference.url).await?;
        Ok(DownloadLocation {
            url,
            expires_at: None,
        })
    }

    async fn read_remote_stream(
        &self,
        hash: &str,
        pack: bool,
    ) -> Result<StorageByteStream, StorageError> {
        if !self.direct_download_proven {
            return Err(StorageError::Unavailable(format!(
                "{} direct download is not proven; provider is upload-only",
                self.kind.provider_type()
            )));
        }
        let reference = self.state_reference(hash, pack)?;
        let url = self.resolve_filemirage_url(&reference.url).await?;
        let _slot = self.acquire_slot().await?;
        let response = self
            .auth(self.client.get(url))
            .send()
            .await
            .map_err(|error| {
                StorageError::NetworkUnavailable(format!(
                    "{} streaming download failed: {error}",
                    self.kind.provider_type()
                ))
            })?;
        if !response.status().is_success() {
            return Err(StorageError::Provider(format!(
                "{} streaming download returned HTTP {}",
                self.kind.provider_type(),
                response.status().as_u16()
            )));
        }
        let provider_type = self.kind.provider_type().to_owned();
        Ok(Box::pin(response.bytes_stream().map(move |result| {
            result.map_err(|error| {
                StorageError::NetworkUnavailable(format!(
                    "{provider_type} streaming body failed: {error}"
                ))
            })
        })))
    }
}

#[async_trait]
impl StorageProvider for FileMirageStorage {
    fn provider_id(&self) -> &str {
        &self.inner.provider_id
    }
    fn tier(&self) -> StorageTier {
        StorageTier::Hot
    }
    fn provider_type(&self) -> &str {
        HttpHotKind::FileMirage.provider_type()
    }
    fn failure_domain(&self) -> &str {
        HttpHotKind::FileMirage.provider_type()
    }
    fn capabilities(&self) -> StorageProviderCapabilities {
        self.inner.capabilities_for()
    }
    async fn put_encoded(&self, hash: &str, bytes: &[u8]) -> Result<(), StorageError> {
        validate_hash(hash)?;
        verify_encoded_bytes(hash, bytes)?;
        self.inner
            .upload_filemirage_bytes(hash, bytes, &format!("{hash}.bin"), false)
            .await
    }
    async fn read_encoded(&self, hash: &str) -> Result<Vec<u8>, StorageError> {
        validate_hash(hash)?;
        let bytes = self.inner.read_remote(hash, false).await?;
        verify_encoded_bytes(hash, &bytes)?;
        Ok(bytes)
    }
    async fn delete_encoded(&self, hash: &str) -> Result<(), StorageError> {
        self.inner.delete_remote(hash, false).await
    }
    async fn download_location(&self, hash: &str) -> Result<DownloadLocation, StorageError> {
        self.inner.download_location_for(hash, false).await
    }
    async fn put_pack(&self, hash: &str, bytes: &[u8]) -> Result<(), StorageError> {
        validate_pack_hash(hash)?;
        verify_pack_bytes(hash, bytes)?;
        self.inner
            .upload_filemirage_bytes(hash, bytes, &format!("{hash}.pack"), true)
            .await
    }
    async fn put_pack_file(&self, hash: &str, path: &Path) -> Result<(), StorageError> {
        validate_pack_hash(hash)?;
        self.inner
            .upload_filemirage_file(hash, path, &format!("{hash}.pack"), true)
            .await
    }
    async fn read_pack(&self, hash: &str) -> Result<Vec<u8>, StorageError> {
        validate_pack_hash(hash)?;
        let bytes = self.inner.read_remote(hash, true).await?;
        verify_pack_bytes(hash, &bytes)?;
        Ok(bytes)
    }
    async fn read_pack_stream(&self, hash: &str) -> Result<StorageByteStream, StorageError> {
        validate_pack_hash(hash)?;
        self.inner.read_remote_stream(hash, true).await
    }
    async fn delete_pack(&self, hash: &str) -> Result<(), StorageError> {
        self.inner.delete_remote(hash, true).await
    }
    async fn download_pack_location(&self, hash: &str) -> Result<DownloadLocation, StorageError> {
        self.inner.download_location_for(hash, true).await
    }
    async fn health_check(&self) -> Result<(), StorageError> {
        self.inner.health().await
    }
}

#[async_trait]
impl StorageProvider for BuzzheavierStorage {
    fn provider_id(&self) -> &str {
        &self.inner.provider_id
    }
    fn tier(&self) -> StorageTier {
        StorageTier::Hot
    }
    fn provider_type(&self) -> &str {
        HttpHotKind::Buzzheavier.provider_type()
    }
    fn failure_domain(&self) -> &str {
        HttpHotKind::Buzzheavier.provider_type()
    }
    fn capabilities(&self) -> StorageProviderCapabilities {
        self.inner.capabilities_for()
    }
    async fn put_encoded(&self, hash: &str, bytes: &[u8]) -> Result<(), StorageError> {
        validate_hash(hash)?;
        verify_encoded_bytes(hash, bytes)?;
        self.inner
            .upload_buzzheavier_bytes(hash, bytes, &format!("{hash}.bin"), false)
            .await
    }
    async fn read_encoded(&self, hash: &str) -> Result<Vec<u8>, StorageError> {
        validate_hash(hash)?;
        let bytes = self.inner.read_remote(hash, false).await?;
        verify_encoded_bytes(hash, &bytes)?;
        Ok(bytes)
    }
    async fn delete_encoded(&self, hash: &str) -> Result<(), StorageError> {
        self.inner.delete_remote(hash, false).await
    }
    async fn download_location(&self, hash: &str) -> Result<DownloadLocation, StorageError> {
        self.inner.download_location_for(hash, false).await
    }
    async fn put_pack(&self, hash: &str, bytes: &[u8]) -> Result<(), StorageError> {
        validate_pack_hash(hash)?;
        verify_pack_bytes(hash, bytes)?;
        self.inner
            .upload_buzzheavier_bytes(hash, bytes, &format!("{hash}.pack"), true)
            .await
    }
    async fn put_pack_file(&self, hash: &str, path: &Path) -> Result<(), StorageError> {
        validate_pack_hash(hash)?;
        self.inner.upload_buzzheavier_file(hash, path, true).await
    }
    async fn read_pack(&self, hash: &str) -> Result<Vec<u8>, StorageError> {
        validate_pack_hash(hash)?;
        let bytes = self.inner.read_remote(hash, true).await?;
        verify_pack_bytes(hash, &bytes)?;
        Ok(bytes)
    }
    async fn read_pack_stream(&self, hash: &str) -> Result<StorageByteStream, StorageError> {
        validate_pack_hash(hash)?;
        self.inner.read_remote_stream(hash, true).await
    }
    async fn delete_pack(&self, hash: &str) -> Result<(), StorageError> {
        self.inner.delete_remote(hash, true).await
    }
    async fn download_pack_location(&self, hash: &str) -> Result<DownloadLocation, StorageError> {
        self.inner.download_location_for(hash, true).await
    }
    async fn health_check(&self) -> Result<(), StorageError> {
        self.inner.health().await
    }
}

fn extract_filemirage_direct_url(html: &str) -> Option<String> {
    let marker = "window .location.href = \"";
    let start = html.find(marker)? + marker.len();
    let remainder = &html[start..];
    let end = remainder.find('"')?;
    let url = &remainder[..end];
    (url.starts_with("http://") || url.starts_with("https://")).then(|| url.to_owned())
}

fn extract_url(body: &[u8], server: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    let raw = value
        .pointer("/data/url")
        .and_then(serde_json::Value::as_str)
        .or_else(|| value.pointer("/url").and_then(serde_json::Value::as_str))?;
    if raw.starts_with("http://") || raw.starts_with("https://") {
        Some(raw.to_owned())
    } else if raw.starts_with('/') {
        Some(format!("{server}{raw}"))
    } else {
        Some(format!("{server}/{raw}"))
    }
}

fn validate_common_config(
    provider_id: &str,
    base_url: &str,
    upload_chunk_bytes: usize,
    max_concurrent_requests: usize,
) -> Result<(), StorageError> {
    if provider_id.trim().is_empty() || base_url.trim().is_empty() {
        return Err(StorageError::Configuration(
            "HTTP HOT provider ID and base URL are required".to_owned(),
        ));
    }
    if upload_chunk_bytes > 0 && upload_chunk_bytes < 1024 * 1024 {
        return Err(StorageError::Configuration(
            "HTTP HOT upload chunk size must be at least 1 MiB".to_owned(),
        ));
    }
    if max_concurrent_requests == 0 {
        return Err(StorageError::Configuration(
            "HTTP HOT request concurrency must be positive".to_owned(),
        ));
    }
    Ok(())
}

fn env_string(name: &str, default: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| default.to_owned())
}

fn env_optional(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn env_path(name: &str, default: PathBuf) -> PathBuf {
    std::env::var_os(name).map(PathBuf::from).unwrap_or(default)
}

fn env_u64(name: &str, default: u64) -> Result<u64, StorageError> {
    std::env::var(name)
        .unwrap_or_else(|_| default.to_string())
        .parse()
        .map_err(|error| {
            StorageError::Configuration(format!("{name} must be an unsigned integer: {error}"))
        })
}

fn env_usize(name: &str, default: usize) -> Result<usize, StorageError> {
    let value = env_u64(name, default as u64)?;
    usize::try_from(value)
        .map_err(|error| StorageError::Configuration(format!("{name} is too large: {error}")))
}

fn env_bool(name: &str, default: bool) -> bool {
    std::env::var(name)
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(default)
}

fn unique_suffix() -> String {
    format!(
        "{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_file(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("vaultnode-{name}-{}.json", unique_suffix()))
    }

    #[test]
    fn filemirage_defaults_expose_only_observed_hot_capabilities() {
        let storage = FileMirageStorage::new(FileMirageStorageConfig {
            provider_id: "filemirage".to_owned(),
            base_url: "https://filemirage.example".to_owned(),
            upload_server_url: None,
            api_token: None,
            state_file: state_file("filemirage-capabilities"),
            upload_chunk_bytes: 99 * 1024 * 1024,
            request_timeout: Duration::from_secs(30),
            max_concurrent_requests: 4,
            delete_proven: false,
        })
        .unwrap();
        let capabilities = storage.capabilities();
        assert!(capabilities.upload);
        assert!(capabilities.direct_download);
        assert!(capabilities.range_requests);
        assert!(!capabilities.delete);
        assert!(!capabilities.stable_urls);
        assert!(!capabilities.expiring_urls);
    }

    #[test]
    fn buzzheavier_defaults_are_upload_only_until_download_is_proven() {
        let storage = BuzzheavierStorage::new(BuzzheavierStorageConfig {
            provider_id: "buzzheavier".to_owned(),
            upload_base_url: "https://w.buzzheavier.example".to_owned(),
            download_base_url: "https://buzzheavier.example".to_owned(),
            account_id: None,
            state_file: state_file("buzz-capabilities"),
            request_timeout: Duration::from_secs(30),
            max_concurrent_requests: 2,
            direct_download_proven: false,
            range_requests_proven: false,
            delete_proven: false,
        })
        .unwrap();
        let capabilities = storage.capabilities();
        assert!(capabilities.upload);
        assert!(!capabilities.direct_download);
        assert!(!capabilities.range_requests);
        assert!(!capabilities.delete);
    }

    #[test]
    fn buzzheavier_rejects_range_without_direct_download() {
        let error = BuzzheavierStorage::new(BuzzheavierStorageConfig {
            provider_id: "buzzheavier".to_owned(),
            upload_base_url: "https://w.buzzheavier.example".to_owned(),
            download_base_url: "https://buzzheavier.example".to_owned(),
            account_id: None,
            state_file: state_file("buzz-invalid"),
            request_timeout: Duration::from_secs(30),
            max_concurrent_requests: 2,
            direct_download_proven: false,
            range_requests_proven: true,
            delete_proven: false,
        })
        .unwrap_err();
        assert!(error.to_string().contains("range support"));
    }
}
