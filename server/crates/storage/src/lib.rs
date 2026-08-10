use async_trait::async_trait;
use aws_credential_types::Credentials;
use aws_sdk_s3::{
    Client as S3Client,
    config::{BehaviorVersion, Region, retry::RetryConfig},
    error::{ProvideErrorMetadata, SdkError},
    presigning::PresigningConfig,
    primitives::ByteStream,
    types::{CompletedMultipartUpload, CompletedPart},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
use std::{collections::HashMap, env};
use thiserror::Error;
use tokio::sync::Semaphore;

mod tiering;

pub use tiering::{
    CapacityReservationStore, CapacitySnapshot, ExistingStorageReplica, FakeMegaAccount,
    FakeMegaFailure, InMemoryCapacityReservationStore, ManualStorageCapacityProvisioner,
    MegaAccountBackend, MegaAccountConfig, MegaCliAccount, MegaColdStorageConfig,
    MegaColdStorageOptions, MegaColdStoragePool, PlacementPool, PlacementProvider,
    ProvisionedCapacity, ProvisioningMode, RestoreMode, StorageAccountSnapshot,
    StorageAccountStatus, StorageCapacityManager, StorageCapacityProvisioner, StorageClass,
    StoragePlacementAction, StoragePlacementEngine, StoragePlacementPlan, StoragePolicy,
    StoragePool, StoragePoolCandidate, StoragePoolHealth, StoragePoolMetadata, StoragePoolStatus,
    StorageReservation, StorageTier,
};

const OBJECT_PREFIX: &str = "chunks/encoded/";
const MIN_MULTIPART_PART_BYTES: usize = 5 * 1024 * 1024;
const MAX_PRESIGN_SECONDS: u64 = 7 * 24 * 60 * 60;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("invalid object hash")]
    InvalidHash,
    #[error("storage configuration error: {0}")]
    Configuration(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("object hash mismatch: expected {expected}, got {actual}")]
    HashMismatch { expected: String, actual: String },
    #[error("storage provider error: {0}")]
    Provider(String),
    #[error("storage rate limiter closed")]
    RateLimiterClosed,
    #[error("deterministic storage failure injection")]
    InjectedFailure,
    #[error(
        "storage capacity exhausted: required {required_bytes} bytes, available {available_bytes} bytes"
    )]
    NeedsCapacity {
        required_bytes: u64,
        available_bytes: u64,
    },
    #[error("storage account authentication failed: {0}")]
    Authentication(String),
    #[error("MEGA network unavailable: {0}")]
    NetworkUnavailable(String),
    #[error("storage account unavailable: {0}")]
    Unavailable(String),
    #[error("storage pool is unavailable")]
    PoolUnavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DownloadLocation {
    pub url: String,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StorageProviderHealth {
    pub provider: String,
    pub pool_id: String,
    pub failure_domain: String,
    pub storage_class: StorageClass,
    pub tier: StorageTier,
    pub healthy: bool,
    pub error: Option<String>,
}

#[async_trait]
pub trait StorageProvider: Send + Sync {
    fn provider_id(&self) -> &str;
    fn tier(&self) -> StorageTier;
    fn storage_class(&self) -> StorageClass {
        self.tier()
    }
    fn pool_id(&self) -> &str {
        self.provider_id()
    }
    fn provider_type(&self) -> &str {
        self.provider_id()
    }
    fn failure_domain(&self) -> &str {
        self.provider_id()
    }
    async fn put_encoded(&self, encoded_hash: &str, bytes: &[u8]) -> Result<(), StorageError>;
    async fn head_encoded(&self, encoded_hash: &str) -> Result<Option<u64>, StorageError> {
        match self.read_encoded(encoded_hash).await {
            Ok(bytes) => Ok(Some(bytes.len() as u64)),
            Err(StorageError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }
    async fn read_encoded(&self, encoded_hash: &str) -> Result<Vec<u8>, StorageError>;
    async fn delete_encoded(&self, encoded_hash: &str) -> Result<(), StorageError>;
    async fn download_location(&self, encoded_hash: &str)
    -> Result<DownloadLocation, StorageError>;
    async fn health_check(&self) -> Result<(), StorageError> {
        Ok(())
    }
    async fn cleanup_orphaned_multipart_uploads(&self) -> Result<u64, StorageError> {
        Ok(0)
    }
}

#[derive(Clone, Default)]
pub struct StorageRegistry {
    providers: Arc<Vec<Arc<dyn StorageProvider>>>,
    pools: Arc<Vec<StoragePool>>,
    provider_pool_ids: Arc<HashMap<String, String>>,
}

impl StorageRegistry {
    pub fn new(providers: Vec<Arc<dyn StorageProvider>>) -> Result<Self, StorageError> {
        let mut pools = Vec::new();
        for provider in &providers {
            if pools
                .iter()
                .any(|pool: &StoragePool| pool.id == provider.pool_id())
            {
                continue;
            }
            pools.push(StoragePool::for_provider(
                provider.pool_id().to_owned(),
                provider.storage_class(),
                provider.provider_type().to_owned(),
                provider.failure_domain().to_owned(),
            ));
        }
        Self::with_pools(providers, pools)
    }

    pub fn with_pools(
        providers: Vec<Arc<dyn StorageProvider>>,
        pools: Vec<StoragePool>,
    ) -> Result<Self, StorageError> {
        let provider_pool_ids = providers
            .iter()
            .map(|provider| {
                (
                    provider.provider_id().to_owned(),
                    provider.pool_id().to_owned(),
                )
            })
            .collect();
        Self::with_pool_mapping(providers, pools, provider_pool_ids)
    }

    pub fn with_provider_pools(
        entries: Vec<(Arc<dyn StorageProvider>, StoragePool)>,
    ) -> Result<Self, StorageError> {
        let mut providers = Vec::with_capacity(entries.len());
        let mut pools = Vec::with_capacity(entries.len());
        let mut provider_pool_ids = HashMap::new();
        for (provider, pool) in entries {
            provider_pool_ids.insert(provider.provider_id().to_owned(), pool.id.clone());
            if !pools
                .iter()
                .any(|candidate: &StoragePool| candidate.id == pool.id)
            {
                pools.push(pool);
            }
            providers.push(provider);
        }
        Self::with_pool_mapping(providers, pools, provider_pool_ids)
    }

    fn with_pool_mapping(
        providers: Vec<Arc<dyn StorageProvider>>,
        mut pools: Vec<StoragePool>,
        mut provider_pool_ids: HashMap<String, String>,
    ) -> Result<Self, StorageError> {
        if providers.is_empty() {
            return Err(StorageError::Configuration(
                "at least one storage provider is required".to_owned(),
            ));
        }
        let mut pool_ids = std::collections::HashSet::new();
        for pool in &pools {
            if pool.id.trim().is_empty()
                || pool.failure_domain.trim().is_empty()
                || !pool_ids.insert(pool.id.clone())
            {
                return Err(StorageError::Configuration(
                    "storage pools require unique IDs and failure domains".to_owned(),
                ));
            }
        }
        for provider in &providers {
            let pool_id = provider_pool_ids
                .entry(provider.provider_id().to_owned())
                .or_insert_with(|| provider.pool_id().to_owned())
                .clone();
            if !pools.iter().any(|pool| pool.id == pool_id) {
                pools.push(StoragePool::for_provider(
                    pool_id,
                    provider.storage_class(),
                    provider.provider_type().to_owned(),
                    provider.failure_domain().to_owned(),
                ));
            }
        }
        pools.sort_by(|left, right| {
            left.priority
                .cmp(&right.priority)
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(Self {
            providers: Arc::new(providers),
            pools: Arc::new(pools),
            provider_pool_ids: Arc::new(provider_pool_ids),
        })
    }

    pub fn providers(&self) -> &[Arc<dyn StorageProvider>] {
        self.providers.as_ref()
    }

    pub fn providers_for_tier(&self, tier: StorageTier) -> Vec<Arc<dyn StorageProvider>> {
        self.providers
            .iter()
            .filter(|provider| {
                self.pool_for_provider(provider.provider_id())
                    .is_some_and(|pool| pool.storage_class == tier)
            })
            .cloned()
            .collect()
    }

    pub fn providers_for_class(&self, class: StorageClass) -> Vec<Arc<dyn StorageProvider>> {
        self.providers_for_tier(class)
    }

    pub fn pools(&self) -> &[StoragePool] {
        self.pools.as_ref()
    }

    pub fn pool(&self, pool_id: &str) -> Option<&StoragePool> {
        self.pools.iter().find(|pool| pool.id == pool_id)
    }

    pub fn pool_for_provider(&self, provider_id: &str) -> Option<&StoragePool> {
        let pool_id = self.provider_pool_ids.get(provider_id)?;
        self.pool(pool_id)
    }

    pub fn providers_for_pool(&self, pool_id: &str) -> Vec<Arc<dyn StorageProvider>> {
        self.providers
            .iter()
            .filter(|provider| {
                self.pool_for_provider(provider.provider_id())
                    .is_some_and(|pool| pool.id == pool_id)
            })
            .cloned()
            .collect()
    }

    pub fn restore_sources(&self, class: StorageClass) -> Vec<Arc<dyn StorageProvider>> {
        let mut providers = self
            .providers
            .iter()
            .filter(|provider| {
                self.pool_for_provider(provider.provider_id())
                    .is_some_and(|pool| pool.storage_class == class && pool.enabled)
            })
            .cloned()
            .collect::<Vec<_>>();
        providers.sort_by(|left, right| {
            let left_pool = self.pool_for_provider(left.provider_id());
            let right_pool = self.pool_for_provider(right.provider_id());
            left_pool
                .map(|pool| pool.priority)
                .unwrap_or(i32::MAX)
                .cmp(&right_pool.map(|pool| pool.priority).unwrap_or(i32::MAX))
                .then_with(|| left.provider_id().cmp(right.provider_id()))
        });
        providers
    }

    pub async fn read_from_restore_source(
        &self,
        encoded_hash: &str,
        class: StorageClass,
    ) -> Result<(String, Vec<u8>), StorageError> {
        let mut last_error = None;
        for provider in self.restore_sources(class) {
            match provider.read_encoded(encoded_hash).await {
                Ok(bytes) => return Ok((provider.provider_id().to_owned(), bytes)),
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error.unwrap_or_else(|| {
            StorageError::Unavailable(format!(
                "no {} restore source is configured",
                class.as_str()
            ))
        }))
    }

    pub async fn placement_pools(&self) -> Vec<StoragePoolCandidate> {
        let mut candidates = Vec::with_capacity(self.providers.len());
        for provider in self.providers.iter() {
            let Some(pool) = self.pool_for_provider(provider.provider_id()) else {
                continue;
            };
            let health = provider.health_check().await;
            let healthy = health.is_ok();
            let status = if !pool.enabled {
                StoragePoolStatus::Disabled
            } else if healthy {
                pool.status
            } else {
                StoragePoolStatus::Unavailable
            };
            candidates.push(StoragePoolCandidate {
                pool_id: pool.id.clone(),
                provider_id: provider.provider_id().to_owned(),
                storage_class: pool.storage_class,
                provider_type: pool.provider_type.clone(),
                priority: pool.priority,
                failure_domain: pool.failure_domain.clone(),
                enabled: pool.enabled,
                status,
                healthy,
                capacity_available_bytes: None,
            });
        }
        candidates
    }

    pub fn provider(&self, provider_id: &str) -> Option<Arc<dyn StorageProvider>> {
        self.providers
            .iter()
            .find(|provider| provider.provider_id() == provider_id)
            .cloned()
    }

    pub async fn put_encoded(&self, encoded_hash: &str, bytes: &[u8]) -> Result<(), StorageError> {
        for provider in self.providers.iter() {
            provider.put_encoded(encoded_hash, bytes).await?;
        }
        Ok(())
    }

    pub async fn download_locations(
        &self,
        encoded_hash: &str,
    ) -> Result<Vec<DownloadLocation>, StorageError> {
        self.download_locations_for_tier(encoded_hash, None).await
    }

    pub async fn download_locations_for_tier(
        &self,
        encoded_hash: &str,
        tier: Option<StorageTier>,
    ) -> Result<Vec<DownloadLocation>, StorageError> {
        validate_hash(encoded_hash)?;
        let mut locations = Vec::with_capacity(self.providers.len());
        for provider in self.providers.iter() {
            if tier.is_some_and(|expected| {
                self.pool_for_provider(provider.provider_id())
                    .is_none_or(|pool| pool.storage_class != expected)
            }) {
                continue;
            }
            if let Ok(location) = provider.download_location(encoded_hash).await
                && !locations
                    .iter()
                    .any(|existing: &DownloadLocation| existing.url == location.url)
            {
                locations.push(location);
            }
        }
        Ok(locations)
    }

    pub async fn health(&self) -> Vec<StorageProviderHealth> {
        let mut health = Vec::with_capacity(self.providers.len());
        for provider in self.providers.iter() {
            let pool = self.pool_for_provider(provider.provider_id());
            let storage_class = pool
                .map(|pool| pool.storage_class)
                .unwrap_or_else(|| provider.storage_class());
            let pool_id = pool
                .map(|pool| pool.id.clone())
                .unwrap_or_else(|| provider.pool_id().to_owned());
            let failure_domain = pool
                .map(|pool| pool.failure_domain.clone())
                .unwrap_or_else(|| provider.failure_domain().to_owned());
            let result = match pool {
                Some(pool)
                    if !pool.enabled || matches!(pool.status, StoragePoolStatus::Disabled) =>
                {
                    Err(StorageError::Unavailable(format!(
                        "storage pool {} is disabled",
                        pool.id
                    )))
                }
                _ => provider.health_check().await,
            };
            health.push(StorageProviderHealth {
                provider: provider.provider_id().to_owned(),
                pool_id,
                failure_domain,
                storage_class,
                tier: storage_class,
                healthy: result.is_ok(),
                error: result.err().map(|error| error.to_string()),
            });
        }
        health
    }

    pub async fn cleanup_orphaned_multipart_uploads(&self) -> Result<u64, StorageError> {
        let mut cleaned = 0;
        for provider in self.providers.iter() {
            cleaned += provider.cleanup_orphaned_multipart_uploads().await?;
        }
        Ok(cleaned)
    }
}

#[derive(Debug, Clone)]
pub struct LocalStorage {
    root: PathBuf,
    base_url: String,
    tier: StorageTier,
    failure_injection: Option<Arc<AtomicUsize>>,
}

impl LocalStorage {
    pub fn new(root: impl Into<PathBuf>, base_url: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            base_url: base_url.into(),
            tier: StorageTier::Hot,
            failure_injection: None,
        }
    }

    pub fn with_tier(
        root: impl Into<PathBuf>,
        base_url: impl Into<String>,
        tier: StorageTier,
    ) -> Self {
        Self {
            root: root.into(),
            base_url: base_url.into(),
            tier,
            failure_injection: None,
        }
    }

    pub fn with_failure_injection(
        root: impl Into<PathBuf>,
        base_url: impl Into<String>,
        successful_uploads_before_failure: usize,
    ) -> Self {
        Self {
            root: root.into(),
            base_url: base_url.into(),
            tier: StorageTier::Hot,
            failure_injection: Some(Arc::new(AtomicUsize::new(
                successful_uploads_before_failure,
            ))),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn object_path(&self, encoded_hash: &str) -> Result<PathBuf, StorageError> {
        validate_hash(encoded_hash)?;
        Ok(self
            .root
            .join(OBJECT_PREFIX)
            .join(format!("{encoded_hash}.bin")))
    }

    pub async fn exists(&self, encoded_hash: &str) -> Result<bool, StorageError> {
        Ok(tokio::fs::try_exists(self.object_path(encoded_hash)?).await?)
    }

    pub async fn cleanup_partials(&self) -> Result<u64, StorageError> {
        let directory = self.root.join(OBJECT_PREFIX);
        let mut removed = 0;
        let mut entries = match tokio::fs::read_dir(directory).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(StorageError::Io(error)),
        };
        while let Some(entry) = entries.next_entry().await? {
            if entry.file_type().await?.is_file()
                && entry.file_name().to_string_lossy().ends_with(".part")
            {
                tokio::fs::remove_file(entry.path()).await?;
                removed += 1;
            }
        }
        Ok(removed)
    }

    fn should_inject_failure(&self) -> bool {
        let Some(counter) = &self.failure_injection else {
            return false;
        };
        loop {
            let remaining = counter.load(Ordering::Acquire);
            if remaining == 0 {
                return true;
            }
            if counter
                .compare_exchange(
                    remaining,
                    remaining - 1,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                return false;
            }
        }
    }
}

#[async_trait]
impl StorageProvider for LocalStorage {
    fn provider_id(&self) -> &str {
        "local"
    }

    async fn put_encoded(&self, encoded_hash: &str, bytes: &[u8]) -> Result<(), StorageError> {
        validate_hash(encoded_hash)?;
        let actual = blake3::hash(bytes).to_hex().to_string();
        if actual != encoded_hash {
            return Err(StorageError::HashMismatch {
                expected: encoded_hash.to_owned(),
                actual,
            });
        }
        let path = self.object_path(encoded_hash)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        if tokio::fs::try_exists(&path).await? {
            match self.read_encoded(encoded_hash).await {
                Ok(_) => return Ok(()),
                Err(StorageError::HashMismatch { .. }) => tokio::fs::remove_file(&path).await?,
                Err(error) => return Err(error),
            }
        }
        let temp = path.with_extension(format!("{}.{}.part", std::process::id(), unique_suffix()));
        tokio::fs::write(&temp, bytes).await?;
        if self.should_inject_failure() {
            return Err(StorageError::InjectedFailure);
        }
        match tokio::fs::rename(&temp, &path).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let _ = tokio::fs::remove_file(&temp).await;
                self.read_encoded(encoded_hash).await?;
            }
            Err(error) => {
                let _ = tokio::fs::remove_file(&temp).await;
                return Err(StorageError::Io(error));
            }
        }
        Ok(())
    }

    async fn read_encoded(&self, encoded_hash: &str) -> Result<Vec<u8>, StorageError> {
        let bytes = tokio::fs::read(self.object_path(encoded_hash)?).await?;
        verify_encoded_bytes(encoded_hash, &bytes)?;
        Ok(bytes)
    }

    async fn head_encoded(&self, encoded_hash: &str) -> Result<Option<u64>, StorageError> {
        let path = self.object_path(encoded_hash)?;
        match tokio::fs::metadata(path).await {
            Ok(metadata) if metadata.is_file() => Ok(Some(metadata.len())),
            Ok(_) => Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(StorageError::Io(error)),
        }
    }

    async fn delete_encoded(&self, encoded_hash: &str) -> Result<(), StorageError> {
        let path = self.object_path(encoded_hash)?;
        match tokio::fs::remove_file(path).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(StorageError::Io(error)),
        }
    }

    async fn download_location(
        &self,
        encoded_hash: &str,
    ) -> Result<DownloadLocation, StorageError> {
        validate_hash(encoded_hash)?;
        Ok(DownloadLocation {
            url: format!(
                "{}/objects/{encoded_hash}",
                self.base_url.trim_end_matches('/')
            ),
            expires_at: None,
        })
    }

    fn tier(&self) -> StorageTier {
        self.tier
    }

    fn provider_type(&self) -> &str {
        "local"
    }

    fn failure_domain(&self) -> &str {
        "local"
    }

    async fn health_check(&self) -> Result<(), StorageError> {
        tokio::fs::create_dir_all(self.root.join(OBJECT_PREFIX)).await?;
        Ok(())
    }
}

#[derive(Clone)]
pub struct S3CompatibleStorageConfig {
    pub provider_id: String,
    pub tier: StorageTier,
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub access_key: String,
    pub secret_key: String,
    pub session_token: Option<String>,
    pub public_base_url: Option<String>,
    pub presign_ttl: Duration,
    pub multipart_threshold_bytes: usize,
    pub multipart_part_bytes: usize,
    pub orphan_multipart_max_age: Duration,
    pub max_attempts: u32,
    pub max_concurrent_requests: usize,
    pub force_path_style: bool,
}

impl std::fmt::Debug for S3CompatibleStorageConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("S3CompatibleStorageConfig")
            .field("provider_id", &self.provider_id)
            .field("tier", &self.tier)
            .field("endpoint", &self.endpoint)
            .field("region", &self.region)
            .field("bucket", &self.bucket)
            .field("access_key", &self.access_key)
            .field("secret_key", &"<redacted>")
            .field(
                "session_token",
                &self.session_token.as_ref().map(|_| "<redacted>"),
            )
            .field("public_base_url", &self.public_base_url)
            .field("presign_ttl", &self.presign_ttl)
            .field("multipart_threshold_bytes", &self.multipart_threshold_bytes)
            .field("multipart_part_bytes", &self.multipart_part_bytes)
            .field("orphan_multipart_max_age", &self.orphan_multipart_max_age)
            .field("max_attempts", &self.max_attempts)
            .field("max_concurrent_requests", &self.max_concurrent_requests)
            .field("force_path_style", &self.force_path_style)
            .finish()
    }
}

impl S3CompatibleStorageConfig {
    pub fn validate(&self) -> Result<(), StorageError> {
        if self.provider_id.trim().is_empty() {
            return Err(StorageError::Configuration(
                "provider ID is required".to_owned(),
            ));
        }
        if self.endpoint.trim().is_empty()
            || self.region.trim().is_empty()
            || self.bucket.trim().is_empty()
        {
            return Err(StorageError::Configuration(
                "endpoint, region, and bucket are required".to_owned(),
            ));
        }
        if self.access_key.trim().is_empty() || self.secret_key.trim().is_empty() {
            return Err(StorageError::Configuration(
                "access key and secret key are required for S3 uploads".to_owned(),
            ));
        }
        if self.presign_ttl.is_zero()
            || self.presign_ttl.as_secs() > MAX_PRESIGN_SECONDS
            || !self.presign_ttl.subsec_nanos().eq(&0)
        {
            return Err(StorageError::Configuration(
                "presign TTL must be a whole number of seconds from 1 to 604800".to_owned(),
            ));
        }
        if self.multipart_part_bytes < MIN_MULTIPART_PART_BYTES {
            return Err(StorageError::Configuration(format!(
                "multipart part size must be at least {MIN_MULTIPART_PART_BYTES} bytes"
            )));
        }
        if self.multipart_threshold_bytes < self.multipart_part_bytes {
            return Err(StorageError::Configuration(
                "multipart threshold must be at least the multipart part size".to_owned(),
            ));
        }
        if self.orphan_multipart_max_age.is_zero() {
            return Err(StorageError::Configuration(
                "orphan multipart max age must be positive".to_owned(),
            ));
        }
        if self.max_attempts == 0 || self.max_concurrent_requests == 0 {
            return Err(StorageError::Configuration(
                "max attempts and max concurrent requests must be positive".to_owned(),
            ));
        }
        Ok(())
    }

    pub fn from_env() -> Result<Self, StorageError> {
        let required = |name: &str| {
            env::var(name).map_err(|_| {
                StorageError::Configuration(format!(
                    "{name} is required when S3 storage is enabled"
                ))
            })
        };
        let parse_u64 = |name: &str, default: u64| -> Result<u64, StorageError> {
            env::var(name)
                .unwrap_or_else(|_| default.to_string())
                .parse::<u64>()
                .map_err(|error| {
                    StorageError::Configuration(format!(
                        "{name} must be an unsigned integer: {error}"
                    ))
                })
        };
        let parse_bool = |name: &str, default: bool| -> Result<bool, StorageError> {
            match env::var(name) {
                Ok(value) => value.parse::<bool>().map_err(|error| {
                    StorageError::Configuration(format!("{name} must be true or false: {error}"))
                }),
                Err(_) => Ok(default),
            }
        };
        let to_usize = |name: &str, value: u64| {
            usize::try_from(value).map_err(|error| {
                StorageError::Configuration(format!("{name} is too large: {error}"))
            })
        };
        let presign_seconds = parse_u64("LAUNCHER_S3_PRESIGN_TTL_SECONDS", 900)?;
        let multipart_threshold =
            parse_u64("LAUNCHER_S3_MULTIPART_THRESHOLD_BYTES", 8 * 1024 * 1024)?;
        let multipart_part = parse_u64("LAUNCHER_S3_MULTIPART_PART_BYTES", 8 * 1024 * 1024)?;
        let orphan_age = parse_u64("LAUNCHER_S3_ORPHAN_MAX_AGE_SECONDS", 86_400)?;
        let max_attempts = parse_u64("LAUNCHER_S3_MAX_ATTEMPTS", 4)?;
        let max_concurrency = parse_u64("LAUNCHER_S3_MAX_CONCURRENT_REQUESTS", 4)?;
        Ok(Self {
            provider_id: env::var("LAUNCHER_S3_PROVIDER_ID").unwrap_or_else(|_| "s3".to_owned()),
            tier: env::var("LAUNCHER_S3_TIER")
                .unwrap_or_else(|_| "HOT".to_owned())
                .parse()?,
            endpoint: required("LAUNCHER_S3_ENDPOINT")?,
            region: required("LAUNCHER_S3_REGION")?,
            bucket: required("LAUNCHER_S3_BUCKET")?,
            access_key: required("LAUNCHER_S3_ACCESS_KEY")?,
            secret_key: required("LAUNCHER_S3_SECRET_KEY")?,
            session_token: env::var("LAUNCHER_S3_SESSION_TOKEN").ok(),
            public_base_url: env::var("LAUNCHER_S3_PUBLIC_BASE_URL")
                .ok()
                .filter(|value| !value.trim().is_empty()),
            presign_ttl: Duration::from_secs(presign_seconds),
            multipart_threshold_bytes: to_usize(
                "LAUNCHER_S3_MULTIPART_THRESHOLD_BYTES",
                multipart_threshold,
            )?,
            multipart_part_bytes: to_usize("LAUNCHER_S3_MULTIPART_PART_BYTES", multipart_part)?,
            orphan_multipart_max_age: Duration::from_secs(orphan_age),
            max_attempts: u32::try_from(max_attempts).map_err(|error| {
                StorageError::Configuration(format!(
                    "LAUNCHER_S3_MAX_ATTEMPTS is too large: {error}"
                ))
            })?,
            max_concurrent_requests: to_usize(
                "LAUNCHER_S3_MAX_CONCURRENT_REQUESTS",
                max_concurrency,
            )?,
            force_path_style: parse_bool("LAUNCHER_S3_FORCE_PATH_STYLE", true)?,
        })
    }
}

pub struct S3CompatibleStorage {
    client: S3Client,
    config: Arc<S3CompatibleStorageConfig>,
    request_slots: Arc<Semaphore>,
}

impl std::fmt::Debug for S3CompatibleStorage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("S3CompatibleStorage")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl S3CompatibleStorage {
    pub fn new(config: S3CompatibleStorageConfig) -> Result<Self, StorageError> {
        config.validate()?;
        let credentials = Credentials::new(
            config.access_key.clone(),
            config.secret_key.clone(),
            config.session_token.clone(),
            None,
            "launcher-storage",
        );
        let sdk_config = aws_sdk_s3::Config::builder()
            .behavior_version(BehaviorVersion::latest())
            .endpoint_url(config.endpoint.clone())
            .region(Region::new(config.region.clone()))
            .credentials_provider(credentials)
            .force_path_style(config.force_path_style)
            .retry_config(RetryConfig::standard().with_max_attempts(config.max_attempts))
            .build();
        Ok(Self {
            client: S3Client::from_conf(sdk_config),
            request_slots: Arc::new(Semaphore::new(config.max_concurrent_requests)),
            config: Arc::new(config),
        })
    }

    pub fn config(&self) -> &S3CompatibleStorageConfig {
        self.config.as_ref()
    }

    fn object_key(&self, encoded_hash: &str) -> Result<String, StorageError> {
        validate_hash(encoded_hash)?;
        Ok(format!("{OBJECT_PREFIX}{encoded_hash}.bin"))
    }

    async fn acquire_request_slot(
        &self,
    ) -> Result<tokio::sync::OwnedSemaphorePermit, StorageError> {
        self.request_slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| StorageError::RateLimiterClosed)
    }

    async fn head_remote(
        &self,
        key: &str,
    ) -> Result<aws_sdk_s3::operation::head_object::HeadObjectOutput, StorageError> {
        self.client
            .head_object()
            .bucket(&self.config.bucket)
            .key(key)
            .send()
            .await
            .map_err(|error| StorageError::Provider(error.to_string()))
    }

    async fn check_bucket(&self) -> Result<(), StorageError> {
        self.client
            .head_bucket()
            .bucket(&self.config.bucket)
            .send()
            .await
            .map(|_| ())
            .map_err(|error| StorageError::Provider(error.to_string()))
    }

    async fn read_remote(&self, key: &str) -> Result<Vec<u8>, StorageError> {
        let response = self
            .client
            .get_object()
            .bucket(&self.config.bucket)
            .key(key)
            .send()
            .await
            .map_err(|error| StorageError::Provider(error.to_string()))?;
        let bytes = response
            .body
            .collect()
            .await
            .map_err(|error| StorageError::Provider(error.to_string()))?
            .into_bytes();
        Ok(bytes.to_vec())
    }

    async fn remote_matches(
        &self,
        encoded_hash: &str,
        key: &str,
        expected_size: usize,
    ) -> Result<bool, StorageError> {
        let head = match self
            .client
            .head_object()
            .bucket(&self.config.bucket)
            .key(key)
            .send()
            .await
        {
            Ok(head) => head,
            Err(error) if Self::is_missing(&error) => {
                return Ok(false);
            }
            Err(error) => return Err(StorageError::Provider(error.to_string())),
        };
        if head.content_length().unwrap_or(-1) != expected_size as i64 {
            return Ok(false);
        }
        if let Some(metadata_hash) = head.metadata().and_then(|metadata| metadata.get("blake3")) {
            return Ok(metadata_hash == encoded_hash);
        }
        let bytes = self.read_remote(key).await?;
        Ok(verify_encoded_bytes(encoded_hash, &bytes).is_ok())
    }

    async fn put_single(
        &self,
        encoded_hash: &str,
        key: &str,
        bytes: &[u8],
    ) -> Result<(), StorageError> {
        self.client
            .put_object()
            .bucket(&self.config.bucket)
            .key(key)
            .content_length(bytes.len() as i64)
            .metadata("blake3", encoded_hash)
            .body(ByteStream::from(bytes.to_vec()))
            .send()
            .await
            .map_err(|error| StorageError::Provider(error.to_string()))?;
        Ok(())
    }

    async fn abort_upload(&self, key: &str, upload_id: &str) -> Result<(), StorageError> {
        self.client
            .abort_multipart_upload()
            .bucket(&self.config.bucket)
            .key(key)
            .upload_id(upload_id)
            .send()
            .await
            .map(|_| ())
            .map_err(|error| StorageError::Provider(error.to_string()))
    }

    async fn put_multipart(
        &self,
        encoded_hash: &str,
        key: &str,
        bytes: &[u8],
    ) -> Result<(), StorageError> {
        let upload = self
            .client
            .create_multipart_upload()
            .bucket(&self.config.bucket)
            .key(key)
            .metadata("blake3", encoded_hash)
            .send()
            .await
            .map_err(|error| StorageError::Provider(error.to_string()))?;
        let upload_id = upload
            .upload_id()
            .ok_or_else(|| {
                StorageError::Provider("S3 did not return a multipart upload ID".to_owned())
            })?
            .to_owned();

        let result = async {
            let mut completed_parts = Vec::with_capacity(
                (bytes.len() / self.config.multipart_part_bytes).saturating_add(1),
            );
            for (index, part) in bytes.chunks(self.config.multipart_part_bytes).enumerate() {
                let part_number = i32::try_from(index + 1).map_err(|_| {
                    StorageError::Configuration("multipart upload exceeds 10000 parts".to_owned())
                })?;
                let output = self
                    .client
                    .upload_part()
                    .bucket(&self.config.bucket)
                    .key(key)
                    .upload_id(&upload_id)
                    .part_number(part_number)
                    .content_length(part.len() as i64)
                    .body(ByteStream::from(part.to_vec()))
                    .send()
                    .await
                    .map_err(|error| StorageError::Provider(error.to_string()))?;
                let etag = output.e_tag().ok_or_else(|| {
                    StorageError::Provider("S3 did not return a multipart ETag".to_owned())
                })?;
                completed_parts.push(
                    CompletedPart::builder()
                        .part_number(part_number)
                        .e_tag(etag)
                        .build(),
                );
            }
            let multipart = CompletedMultipartUpload::builder()
                .set_parts(Some(completed_parts))
                .build();
            self.client
                .complete_multipart_upload()
                .bucket(&self.config.bucket)
                .key(key)
                .upload_id(&upload_id)
                .multipart_upload(multipart)
                .send()
                .await
                .map_err(|error| StorageError::Provider(error.to_string()))?;
            Ok::<(), StorageError>(())
        }
        .await;

        if result.is_err() {
            let _ = self.abort_upload(key, &upload_id).await;
        }
        result
    }

    fn is_missing<E>(error: &SdkError<E>) -> bool
    where
        E: ProvideErrorMetadata,
    {
        error
            .as_service_error()
            .and_then(ProvideErrorMetadata::code)
            .is_some_and(|code| matches!(code, "NoSuchKey" | "NotFound" | "NoSuchBucket"))
            || error
                .raw_response()
                .is_some_and(|response| response.status().as_u16() == 404)
    }

    async fn cleanup_multipart_page(
        &self,
        key_marker: Option<&str>,
        upload_id_marker: Option<&str>,
    ) -> Result<(u64, Option<String>, Option<String>, bool), StorageError> {
        let mut request = self
            .client
            .list_multipart_uploads()
            .bucket(&self.config.bucket)
            .prefix(OBJECT_PREFIX);
        if let Some(key_marker) = key_marker {
            request = request.key_marker(key_marker);
        }
        if let Some(upload_id_marker) = upload_id_marker {
            request = request.upload_id_marker(upload_id_marker);
        }
        let response = request
            .send()
            .await
            .map_err(|error| StorageError::Provider(error.to_string()))?;
        let cutoff = Utc::now()
            - chrono::Duration::from_std(self.config.orphan_multipart_max_age)
                .map_err(|error| StorageError::Configuration(error.to_string()))?;
        let uploads = response
            .uploads()
            .iter()
            .filter_map(|upload| {
                let initiated = upload.initiated()?;
                if initiated.secs() > cutoff.timestamp() {
                    return None;
                }
                Some((upload.key()?.to_owned(), upload.upload_id()?.to_owned()))
            })
            .collect::<Vec<_>>();
        let mut cleaned = 0;
        for (key, upload_id) in uploads {
            self.abort_upload(&key, &upload_id).await?;
            cleaned += 1;
        }
        Ok((
            cleaned,
            response.next_key_marker().map(str::to_owned),
            response.next_upload_id_marker().map(str::to_owned),
            response.is_truncated().unwrap_or(false),
        ))
    }
}

#[async_trait]
impl StorageProvider for S3CompatibleStorage {
    fn provider_id(&self) -> &str {
        &self.config.provider_id
    }

    fn tier(&self) -> StorageTier {
        self.config.tier
    }

    fn provider_type(&self) -> &str {
        "s3"
    }

    async fn put_encoded(&self, encoded_hash: &str, bytes: &[u8]) -> Result<(), StorageError> {
        validate_hash(encoded_hash)?;
        verify_encoded_bytes(encoded_hash, bytes)?;
        let _slot = self.acquire_request_slot().await?;
        let key = self.object_key(encoded_hash)?;
        if self.remote_matches(encoded_hash, &key, bytes.len()).await? {
            return Ok(());
        }
        if bytes.len() >= self.config.multipart_threshold_bytes {
            self.put_multipart(encoded_hash, &key, bytes).await?;
        } else {
            self.put_single(encoded_hash, &key, bytes).await?;
        }
        let head = self.head_remote(&key).await?;
        if head.content_length().unwrap_or(-1) != bytes.len() as i64
            || head
                .metadata()
                .and_then(|metadata| metadata.get("blake3"))
                .is_some_and(|metadata_hash| metadata_hash != encoded_hash)
        {
            return Err(StorageError::Provider(
                "S3 object verification failed after upload".to_owned(),
            ));
        }
        let remote = self.read_remote(&key).await?;
        verify_encoded_bytes(encoded_hash, &remote)
    }

    async fn read_encoded(&self, encoded_hash: &str) -> Result<Vec<u8>, StorageError> {
        let _slot = self.acquire_request_slot().await?;
        let key = self.object_key(encoded_hash)?;
        let bytes = self.read_remote(&key).await?;
        verify_encoded_bytes(encoded_hash, &bytes)?;
        Ok(bytes)
    }

    async fn head_encoded(&self, encoded_hash: &str) -> Result<Option<u64>, StorageError> {
        let _slot = self.acquire_request_slot().await?;
        let key = self.object_key(encoded_hash)?;
        match self
            .client
            .head_object()
            .bucket(&self.config.bucket)
            .key(key)
            .send()
            .await
        {
            Ok(head) => Ok(head.content_length().map(|length| length as u64)),
            Err(error) if Self::is_missing(&error) => Ok(None),
            Err(error) => Err(StorageError::Provider(error.to_string())),
        }
    }

    async fn delete_encoded(&self, encoded_hash: &str) -> Result<(), StorageError> {
        let _slot = self.acquire_request_slot().await?;
        let key = self.object_key(encoded_hash)?;
        self.client
            .delete_object()
            .bucket(&self.config.bucket)
            .key(key)
            .send()
            .await
            .map(|_| ())
            .map_err(|error| StorageError::Provider(error.to_string()))
    }

    async fn download_location(
        &self,
        encoded_hash: &str,
    ) -> Result<DownloadLocation, StorageError> {
        let key = self.object_key(encoded_hash)?;
        if let Some(public_base_url) = &self.config.public_base_url {
            return Ok(DownloadLocation {
                url: format!("{}/{}", public_base_url.trim_end_matches('/'), key),
                expires_at: None,
            });
        }
        let presigning = PresigningConfig::expires_in(self.config.presign_ttl)
            .map_err(|error| StorageError::Configuration(error.to_string()))?;
        let request = self
            .client
            .get_object()
            .bucket(&self.config.bucket)
            .key(key)
            .presigned(presigning)
            .await
            .map_err(|error| StorageError::Provider(error.to_string()))?;
        let expires_at = Utc::now()
            + chrono::Duration::from_std(self.config.presign_ttl)
                .map_err(|error| StorageError::Configuration(error.to_string()))?;
        Ok(DownloadLocation {
            url: request.uri().to_owned(),
            expires_at: Some(expires_at),
        })
    }

    async fn health_check(&self) -> Result<(), StorageError> {
        let _slot = self.acquire_request_slot().await?;
        self.check_bucket().await
    }

    async fn cleanup_orphaned_multipart_uploads(&self) -> Result<u64, StorageError> {
        let _slot = self.acquire_request_slot().await?;
        let mut cleaned = 0;
        let mut key_marker = None;
        let mut upload_id_marker = None;
        loop {
            let (page_cleaned, next_key, next_upload, truncated) = self
                .cleanup_multipart_page(key_marker.as_deref(), upload_id_marker.as_deref())
                .await?;
            cleaned += page_cleaned;
            if !truncated {
                break;
            }
            if next_key.is_none() && next_upload.is_none() {
                break;
            }
            key_marker = next_key;
            upload_id_marker = next_upload;
        }
        Ok(cleaned)
    }
}

pub fn storage_from_env(
    storage_root: impl Into<PathBuf>,
    base_url: impl Into<String>,
) -> Result<(StorageRegistry, Option<LocalStorage>), StorageError> {
    let storage_root = storage_root.into();
    let base_url = base_url.into();
    let provider_config = env::var("LAUNCHER_STORAGE_PROVIDERS")
        .or_else(|_| env::var("LAUNCHER_STORAGE_PROVIDER"))
        .unwrap_or_else(|_| "local".to_owned());
    let mut providers: Vec<Arc<dyn StorageProvider>> = Vec::new();
    let mut local_storage = None;
    for provider_name in provider_config
        .split(',')
        .map(str::trim)
        .filter(|provider| !provider.is_empty())
    {
        match provider_name {
            "local" => {
                let tier = env::var("LAUNCHER_LOCAL_TIER")
                    .unwrap_or_else(|_| "HOT".to_owned())
                    .parse()?;
                let local = LocalStorage::with_tier(&storage_root, &base_url, tier);
                local_storage = Some(local.clone());
                providers.push(Arc::new(local));
            }
            "s3" => {
                providers.push(Arc::new(S3CompatibleStorage::new(
                    S3CompatibleStorageConfig::from_env()?,
                )?));
            }
            unknown => {
                return Err(StorageError::Configuration(format!(
                    "unsupported LAUNCHER_STORAGE_PROVIDERS entry {unknown:?}; expected local, s3, or mega (use the async factory for mega)"
                )));
            }
        }
    }
    Ok((StorageRegistry::new(providers)?, local_storage))
}

pub async fn storage_from_env_with_reservation_store(
    storage_root: impl Into<PathBuf>,
    base_url: impl Into<String>,
    ledger: Arc<dyn CapacityReservationStore>,
) -> Result<(StorageRegistry, Option<LocalStorage>), StorageError> {
    let storage_root = storage_root.into();
    let base_url = base_url.into();
    let provider_config = env::var("LAUNCHER_STORAGE_PROVIDERS")
        .or_else(|_| env::var("LAUNCHER_STORAGE_PROVIDER"))
        .unwrap_or_else(|_| "local".to_owned());
    let mut providers: Vec<Arc<dyn StorageProvider>> = Vec::new();
    let mut local_storage = None;
    for provider_name in provider_config
        .split(',')
        .map(str::trim)
        .filter(|provider| !provider.is_empty())
    {
        match provider_name {
            "local" => {
                let tier = env::var("LAUNCHER_LOCAL_TIER")
                    .unwrap_or_else(|_| "HOT".to_owned())
                    .parse()?;
                let local = LocalStorage::with_tier(&storage_root, &base_url, tier);
                local_storage = Some(local.clone());
                providers.push(Arc::new(local));
            }
            "s3" => {
                providers.push(Arc::new(S3CompatibleStorage::new(
                    S3CompatibleStorageConfig::from_env()?,
                )?));
            }
            "mega" => {
                let path = env::var("LAUNCHER_MEGA_ACCOUNTS_FILE").map_err(|_| {
                    StorageError::Configuration(
                        "LAUNCHER_MEGA_ACCOUNTS_FILE is required for the mega provider".to_owned(),
                    )
                })?;
                let config = MegaColdStorageConfig::from_file(path)?;
                let pool =
                    MegaColdStoragePool::from_config_and_register(config, ledger.clone()).await?;
                providers.push(Arc::new(pool));
            }
            unknown => {
                return Err(StorageError::Configuration(format!(
                    "unsupported LAUNCHER_STORAGE_PROVIDERS entry {unknown:?}; expected local, s3, or mega"
                )));
            }
        }
    }
    Ok((StorageRegistry::new(providers)?, local_storage))
}

#[derive(Debug, Clone)]
pub struct MirrorSet {
    base_urls: Arc<Vec<String>>,
}

impl MirrorSet {
    pub fn new<I, S>(base_urls: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let urls = base_urls
            .into_iter()
            .map(Into::into)
            .map(|url| url.trim_end_matches('/').to_owned())
            .filter(|url| !url.is_empty())
            .collect::<Vec<_>>();
        let mut unique = Vec::with_capacity(urls.len());
        for url in urls {
            if !unique.contains(&url) {
                unique.push(url);
            }
        }
        Self {
            base_urls: Arc::new(unique),
        }
    }

    pub fn urls(&self, encoded_hash: &str) -> Result<Vec<String>, StorageError> {
        validate_hash(encoded_hash)?;
        Ok(self
            .base_urls
            .iter()
            .map(|base| format!("{base}/objects/{encoded_hash}"))
            .collect())
    }
}

fn verify_encoded_bytes(encoded_hash: &str, bytes: &[u8]) -> Result<(), StorageError> {
    let actual = blake3::hash(bytes).to_hex().to_string();
    if actual != encoded_hash {
        return Err(StorageError::HashMismatch {
            expected: encoded_hash.to_owned(),
            actual,
        });
    }
    Ok(())
}

fn validate_hash(value: &str) -> Result<(), StorageError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(StorageError::InvalidHash);
    }
    Ok(())
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

    #[tokio::test]
    async fn local_provider_round_trips_and_verifies() {
        let root = std::env::temp_dir().join(format!("launcher-storage-{}", uuid_like()));
        let storage = LocalStorage::new(&root, "http://localhost:8080");
        let data = b"content";
        let hash = blake3::hash(data).to_hex().to_string();
        storage.put_encoded(&hash, data).await.unwrap();
        storage.put_encoded(&hash, data).await.unwrap();
        assert_eq!(storage.read_encoded(&hash).await.unwrap(), data);
        assert!(storage.exists(&hash).await.unwrap());
        tokio::fs::write(storage.object_path(&hash).unwrap(), b"corrupt")
            .await
            .unwrap();
        assert!(matches!(
            storage.read_encoded(&hash).await,
            Err(StorageError::HashMismatch { .. })
        ));
        storage.put_encoded(&hash, data).await.unwrap();
        storage.delete_encoded(&hash).await.unwrap();
        assert!(!storage.exists(&hash).await.unwrap());
        assert!(
            matches!(storage.read_encoded(&hash).await, Err(StorageError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound)
        );
        let location = storage.download_location(&hash).await.unwrap();
        assert_eq!(
            location.url,
            format!("http://localhost:8080/objects/{hash}")
        );
        let _ = tokio::fs::remove_dir_all(root).await;
    }

    #[tokio::test]
    async fn concurrent_uploads_are_idempotent() {
        let root =
            std::env::temp_dir().join(format!("launcher-storage-concurrent-{}", uuid_like()));
        let storage = LocalStorage::new(&root, "http://localhost:8080");
        let data = b"concurrent content";
        let hash = blake3::hash(data).to_hex().to_string();
        let first = storage.put_encoded(&hash, data);
        let second = storage.put_encoded(&hash, data);
        let (first, second) = tokio::join!(first, second);
        first.unwrap();
        second.unwrap();
        assert_eq!(storage.read_encoded(&hash).await.unwrap(), data);
        let _ = tokio::fs::remove_dir_all(root).await;
    }

    #[tokio::test]
    async fn interrupted_upload_leaves_recoverable_partial() {
        let root = std::env::temp_dir().join(format!("launcher-storage-failure-{}", uuid_like()));
        let storage = LocalStorage::with_failure_injection(&root, "http://localhost:8080", 0);
        let data = b"interrupted upload";
        let hash = blake3::hash(data).to_hex().to_string();
        assert!(matches!(
            storage.put_encoded(&hash, data).await,
            Err(StorageError::InjectedFailure)
        ));
        assert_eq!(storage.cleanup_partials().await.unwrap(), 1);
        assert!(!storage.exists(&hash).await.unwrap());
        let _ = tokio::fs::remove_dir_all(root).await;
    }

    #[test]
    fn mirrors_preserve_operator_order_and_reject_invalid_hashes() {
        let mirrors = MirrorSet::new(["http://mirror-a/", "http://mirror-b", "http://mirror-a"]);
        let hash = blake3::hash(b"x").to_hex().to_string();
        assert_eq!(
            mirrors.urls(&hash).unwrap(),
            vec![
                format!("http://mirror-a/objects/{hash}"),
                format!("http://mirror-b/objects/{hash}")
            ]
        );
        assert!(matches!(
            mirrors.urls("bad"),
            Err(StorageError::InvalidHash)
        ));
    }

    #[test]
    fn s3_configuration_rejects_unsafe_multipart_and_expiry_values() {
        let mut config = test_s3_config();
        config.multipart_part_bytes = 1;
        assert!(config.validate().is_err());
        config = test_s3_config();
        config.presign_ttl = Duration::from_secs(MAX_PRESIGN_SECONDS + 1);
        assert!(config.validate().is_err());
    }

    #[test]
    fn registry_requires_a_provider_and_deduplicates_urls() {
        assert!(StorageRegistry::new(Vec::new()).is_err());
        let provider = Arc::new(LocalStorage::new("storage", "https://download.example"));
        let registry = StorageRegistry::new(vec![provider.clone(), provider]).unwrap();
        assert_eq!(registry.providers().len(), 2);
    }

    #[tokio::test]
    async fn registry_keeps_alternate_locations_when_one_provider_is_unavailable() {
        let hash = blake3::hash(b"alternate").to_hex().to_string();
        let registry = StorageRegistry::new(vec![
            Arc::new(UnavailableProvider),
            Arc::new(LocalStorage::new("storage", "https://mirror.example")),
        ])
        .unwrap();
        let locations = registry.download_locations(&hash).await.unwrap();
        assert_eq!(locations.len(), 1);
        assert_eq!(
            locations[0].url,
            format!("https://mirror.example/objects/{hash}")
        );
    }

    #[tokio::test]
    async fn restore_source_selection_falls_back_by_pool_priority() {
        let root = std::env::temp_dir().join(format!("launcher-restore-fallback-{}", uuid_like()));
        let fallback =
            LocalStorage::with_tier(&root, "https://fallback.example", StorageClass::Cold);
        let bytes = b"restore fallback";
        let hash = blake3::hash(bytes).to_hex().to_string();
        fallback.put_encoded(&hash, bytes).await.unwrap();
        let registry = StorageRegistry::with_pools(
            vec![
                Arc::new(ColdUnavailableProvider),
                Arc::new(fallback.clone()),
            ],
            vec![
                StoragePool::for_provider("cold-preferred", StorageClass::Cold, "mega", "mega"),
                StoragePool {
                    id: "local".to_owned(),
                    storage_class: StorageClass::Cold,
                    provider_type: "s3".to_owned(),
                    priority: 200,
                    failure_domain: "archive-provider".to_owned(),
                    enabled: true,
                    status: StoragePoolStatus::Ready,
                    provisioning_mode: ProvisioningMode::Disabled,
                },
            ],
        )
        .unwrap();
        let (provider, restored) = registry
            .read_from_restore_source(&hash, StorageClass::Cold)
            .await
            .unwrap();
        assert_eq!(provider, "local");
        assert_eq!(restored, bytes);
        let _ = tokio::fs::remove_dir_all(root).await;
    }

    #[tokio::test]
    async fn restore_source_selection_reports_when_all_pools_are_unavailable() {
        let registry = StorageRegistry::new(vec![Arc::new(ColdUnavailableProvider)]).unwrap();
        let error = registry
            .read_from_restore_source(&"a".repeat(64), StorageClass::Cold)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("unavailable"));
    }

    struct UnavailableProvider;

    struct ColdUnavailableProvider;

    #[async_trait::async_trait]
    impl StorageProvider for ColdUnavailableProvider {
        fn provider_id(&self) -> &str {
            "cold-preferred"
        }

        fn tier(&self) -> StorageTier {
            StorageClass::Cold
        }

        async fn put_encoded(&self, _: &str, _: &[u8]) -> Result<(), StorageError> {
            Err(StorageError::Provider("unavailable".to_owned()))
        }

        async fn read_encoded(&self, _: &str) -> Result<Vec<u8>, StorageError> {
            Err(StorageError::Unavailable(
                "cold preferred unavailable".to_owned(),
            ))
        }

        async fn delete_encoded(&self, _: &str) -> Result<(), StorageError> {
            Err(StorageError::Provider("unavailable".to_owned()))
        }

        async fn download_location(&self, _: &str) -> Result<DownloadLocation, StorageError> {
            Err(StorageError::Provider("unavailable".to_owned()))
        }
    }

    #[async_trait::async_trait]
    impl StorageProvider for UnavailableProvider {
        fn provider_id(&self) -> &str {
            "unavailable"
        }

        fn tier(&self) -> StorageTier {
            StorageTier::Hot
        }

        async fn put_encoded(&self, _: &str, _: &[u8]) -> Result<(), StorageError> {
            Err(StorageError::Provider("unavailable".to_owned()))
        }

        async fn read_encoded(&self, _: &str) -> Result<Vec<u8>, StorageError> {
            Err(StorageError::Provider("unavailable".to_owned()))
        }

        async fn delete_encoded(&self, _: &str) -> Result<(), StorageError> {
            Err(StorageError::Provider("unavailable".to_owned()))
        }

        async fn download_location(&self, _: &str) -> Result<DownloadLocation, StorageError> {
            Err(StorageError::Provider("unavailable".to_owned()))
        }
    }

    fn test_s3_config() -> S3CompatibleStorageConfig {
        S3CompatibleStorageConfig {
            provider_id: "s3".to_owned(),
            tier: StorageTier::Hot,
            endpoint: "http://127.0.0.1:9000".to_owned(),
            region: "us-east-1".to_owned(),
            bucket: "launcher".to_owned(),
            access_key: "access".to_owned(),
            secret_key: "secret".to_owned(),
            session_token: None,
            public_base_url: None,
            presign_ttl: Duration::from_secs(900),
            multipart_threshold_bytes: 8 * 1024 * 1024,
            multipart_part_bytes: 8 * 1024 * 1024,
            orphan_multipart_max_age: Duration::from_secs(86_400),
            max_attempts: 3,
            max_concurrent_requests: 4,
            force_path_style: true,
        }
    }

    fn uuid_like() -> String {
        format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
    }
}
