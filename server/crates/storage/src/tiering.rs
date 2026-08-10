use super::{DownloadLocation, StorageError, StorageProvider, verify_encoded_bytes};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio::time::timeout;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum StorageClass {
    #[serde(rename = "HOT")]
    Hot,
    #[serde(rename = "COLD")]
    Cold,
    #[serde(rename = "ARCHIVE")]
    Archive,
}

// Compatibility alias. Existing callers and operator commands can continue to
// use StorageTier while the storage domain speaks in logical classes.
pub use StorageClass as StorageTier;

impl StorageClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hot => "HOT",
            Self::Cold => "COLD",
            Self::Archive => "ARCHIVE",
        }
    }

    pub fn all() -> [Self; 3] {
        [Self::Hot, Self::Cold, Self::Archive]
    }
}

impl std::fmt::Display for StorageClass {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for StorageClass {
    type Err = StorageError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_uppercase().as_str() {
            "HOT" => Ok(Self::Hot),
            "COLD" => Ok(Self::Cold),
            "ARCHIVE" => Ok(Self::Archive),
            other => Err(StorageError::Configuration(format!(
                "unknown storage class {other:?}; expected HOT, COLD, or ARCHIVE"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum RestoreMode {
    #[serde(rename = "ON_DEMAND")]
    OnDemand,
    #[serde(rename = "PROACTIVE")]
    Proactive,
}

impl FromStr for RestoreMode {
    type Err = StorageError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_uppercase().as_str() {
            "ON_DEMAND" | "ON-DEMAND" => Ok(Self::OnDemand),
            "PROACTIVE" => Ok(Self::Proactive),
            other => Err(StorageError::Configuration(format!(
                "unknown restore mode {other:?}; expected ON_DEMAND or PROACTIVE"
            ))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoragePolicy {
    pub min_verified_hot_replicas: u32,
    pub min_verified_cold_replicas: u32,
    #[serde(default)]
    pub min_verified_archive_replicas: u32,
    pub preferred_hot_replicas: u32,
    pub preferred_cold_replicas: u32,
    #[serde(default)]
    pub preferred_archive_replicas: u32,
    #[serde(default = "default_hot_failure_domains")]
    pub min_hot_failure_domains: u32,
    #[serde(default)]
    pub min_cold_failure_domains: u32,
    #[serde(default)]
    pub min_archive_failure_domains: u32,
    pub cold_backup_required: bool,
    pub restore_mode: RestoreMode,
}

fn default_hot_failure_domains() -> u32 {
    1
}

impl Default for StoragePolicy {
    fn default() -> Self {
        Self {
            min_verified_hot_replicas: 1,
            min_verified_cold_replicas: 0,
            min_verified_archive_replicas: 0,
            preferred_hot_replicas: 1,
            preferred_cold_replicas: 0,
            preferred_archive_replicas: 0,
            min_hot_failure_domains: 1,
            min_cold_failure_domains: 0,
            min_archive_failure_domains: 0,
            cold_backup_required: false,
            restore_mode: RestoreMode::OnDemand,
        }
    }
}

impl StoragePolicy {
    pub fn staging() -> Self {
        Self {
            min_verified_hot_replicas: 1,
            min_verified_cold_replicas: 1,
            min_verified_archive_replicas: 0,
            preferred_hot_replicas: 1,
            preferred_cold_replicas: 1,
            preferred_archive_replicas: 0,
            min_hot_failure_domains: 1,
            min_cold_failure_domains: 1,
            min_archive_failure_domains: 0,
            cold_backup_required: true,
            restore_mode: RestoreMode::Proactive,
        }
    }

    pub fn validate(&self) -> Result<(), StorageError> {
        if self.min_verified_hot_replicas == 0 {
            return Err(StorageError::Configuration(
                "at least one verified hot replica is required".to_owned(),
            ));
        }
        if self.preferred_hot_replicas < self.min_verified_hot_replicas
            || self.preferred_cold_replicas < self.min_verified_cold_replicas
            || self.preferred_archive_replicas < self.min_verified_archive_replicas
        {
            return Err(StorageError::Configuration(
                "preferred replica counts cannot be below minimum counts".to_owned(),
            ));
        }
        if self.cold_backup_required && self.min_verified_cold_replicas == 0 {
            return Err(StorageError::Configuration(
                "cold backup required must have at least one cold replica".to_owned(),
            ));
        }
        for (class, replicas, domains) in [
            (
                StorageClass::Hot,
                self.min_verified_hot_replicas,
                self.min_hot_failure_domains,
            ),
            (
                StorageClass::Cold,
                self.min_verified_cold_replicas,
                self.min_cold_failure_domains,
            ),
            (
                StorageClass::Archive,
                self.min_verified_archive_replicas,
                self.min_archive_failure_domains,
            ),
        ] {
            if domains > replicas {
                return Err(StorageError::Configuration(format!(
                    "{class} failure-domain requirement {domains} exceeds replica requirement {replicas}"
                )));
            }
        }
        Ok(())
    }

    pub fn from_env() -> Result<Self, StorageError> {
        let parse_u32 = |name: &str, default: u32| -> Result<u32, StorageError> {
            std::env::var(name)
                .unwrap_or_else(|_| default.to_string())
                .parse::<u32>()
                .map_err(|error| {
                    StorageError::Configuration(format!(
                        "{name} must be an unsigned integer: {error}"
                    ))
                })
        };
        let parse_bool = |name: &str, default: bool| -> Result<bool, StorageError> {
            match std::env::var(name) {
                Ok(value) => value.parse::<bool>().map_err(|error| {
                    StorageError::Configuration(format!("{name} must be true or false: {error}"))
                }),
                Err(_) => Ok(default),
            }
        };
        let policy = Self {
            min_verified_hot_replicas: parse_u32("LAUNCHER_STORAGE_MIN_HOT_REPLICAS", 1)?,
            min_verified_cold_replicas: parse_u32("LAUNCHER_STORAGE_MIN_COLD_REPLICAS", 0)?,
            min_verified_archive_replicas: parse_u32("LAUNCHER_STORAGE_MIN_ARCHIVE_REPLICAS", 0)?,
            preferred_hot_replicas: parse_u32("LAUNCHER_STORAGE_PREFERRED_HOT_REPLICAS", 1)?,
            preferred_cold_replicas: parse_u32("LAUNCHER_STORAGE_PREFERRED_COLD_REPLICAS", 0)?,
            preferred_archive_replicas: parse_u32(
                "LAUNCHER_STORAGE_PREFERRED_ARCHIVE_REPLICAS",
                0,
            )?,
            min_hot_failure_domains: parse_u32("LAUNCHER_STORAGE_MIN_HOT_FAILURE_DOMAINS", 1)?,
            min_cold_failure_domains: parse_u32("LAUNCHER_STORAGE_MIN_COLD_FAILURE_DOMAINS", 0)?,
            min_archive_failure_domains: parse_u32(
                "LAUNCHER_STORAGE_MIN_ARCHIVE_FAILURE_DOMAINS",
                0,
            )?,
            cold_backup_required: parse_bool("LAUNCHER_STORAGE_COLD_BACKUP_REQUIRED", false)?,
            restore_mode: std::env::var("LAUNCHER_STORAGE_RESTORE_MODE")
                .unwrap_or_else(|_| "ON_DEMAND".to_owned())
                .parse()?,
        };
        policy.validate()?;
        Ok(policy)
    }

    pub fn required_replicas(&self, tier: StorageTier) -> u32 {
        match tier {
            StorageTier::Hot => self.min_verified_hot_replicas,
            StorageTier::Cold => self
                .min_verified_cold_replicas
                .max(if self.cold_backup_required { 1 } else { 0 }),
            StorageTier::Archive => self.min_verified_archive_replicas,
        }
    }

    pub fn preferred_replicas(&self, tier: StorageTier) -> u32 {
        match tier {
            StorageTier::Hot => self.preferred_hot_replicas,
            StorageTier::Cold => self
                .preferred_cold_replicas
                .max(if self.cold_backup_required { 1 } else { 0 }),
            StorageTier::Archive => self.preferred_archive_replicas,
        }
    }

    pub fn required_failure_domains(&self, class: StorageClass) -> u32 {
        match class {
            StorageClass::Hot => self.min_hot_failure_domains,
            StorageClass::Cold => self.min_cold_failure_domains,
            StorageClass::Archive => self.min_archive_failure_domains,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlacementProvider {
    pub provider_id: String,
    pub tier: StorageTier,
    pub healthy: bool,
    pub capacity_available_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoragePoolCandidate {
    pub pool_id: String,
    pub provider_id: String,
    pub storage_class: StorageClass,
    pub provider_type: String,
    pub priority: i32,
    pub failure_domain: String,
    pub enabled: bool,
    pub status: StoragePoolStatus,
    pub healthy: bool,
    pub capacity_available_bytes: Option<u64>,
}

pub type PlacementPool = StoragePoolCandidate;

impl StoragePoolCandidate {
    pub fn new(
        pool_id: impl Into<String>,
        provider_id: impl Into<String>,
        storage_class: StorageClass,
        provider_type: impl Into<String>,
        priority: i32,
        failure_domain: impl Into<String>,
    ) -> Self {
        Self {
            pool_id: pool_id.into(),
            provider_id: provider_id.into(),
            storage_class,
            provider_type: provider_type.into(),
            priority,
            failure_domain: failure_domain.into(),
            enabled: true,
            status: StoragePoolStatus::Ready,
            healthy: true,
            capacity_available_bytes: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExistingStorageReplica {
    pub provider_id: String,
    pub pool_id: String,
    pub storage_class: StorageClass,
    pub failure_domain: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoragePlacementAction {
    pub provider_id: String,
    pub tier: StorageTier,
    pub pool_id: String,
    pub failure_domain: String,
    pub priority: i32,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoragePlacementPlan {
    pub actions: Vec<StoragePlacementAction>,
    pub existing_hot_replicas: u32,
    pub existing_cold_replicas: u32,
    pub existing_archive_replicas: u32,
    pub existing_hot_failure_domains: u32,
    pub existing_cold_failure_domains: u32,
    pub existing_archive_failure_domains: u32,
    pub projected_hot_replicas: u32,
    pub projected_cold_replicas: u32,
    pub projected_archive_replicas: u32,
    pub projected_hot_failure_domains: u32,
    pub projected_cold_failure_domains: u32,
    pub projected_archive_failure_domains: u32,
    pub required_hot_replicas: u32,
    pub required_cold_replicas: u32,
    pub required_archive_replicas: u32,
    pub required_hot_failure_domains: u32,
    pub required_cold_failure_domains: u32,
    pub required_archive_failure_domains: u32,
    pub policy_satisfied: bool,
    pub explanation: String,
}

#[derive(Debug, Clone)]
pub struct StoragePlacementEngine {
    policy: StoragePolicy,
}

impl StoragePlacementEngine {
    pub fn new(policy: StoragePolicy) -> Result<Self, StorageError> {
        policy.validate()?;
        Ok(Self { policy })
    }

    pub fn policy(&self) -> &StoragePolicy {
        &self.policy
    }

    pub fn plan(
        &self,
        encoded_size: u64,
        existing_provider_ids: &[String],
        providers: &[PlacementProvider],
    ) -> StoragePlacementPlan {
        let candidates = providers
            .iter()
            .map(|provider| StoragePoolCandidate {
                pool_id: provider.provider_id.clone(),
                provider_id: provider.provider_id.clone(),
                storage_class: provider.tier,
                provider_type: provider.provider_id.clone(),
                priority: 100,
                failure_domain: provider.provider_id.clone(),
                enabled: true,
                status: if provider.healthy {
                    StoragePoolStatus::Ready
                } else {
                    StoragePoolStatus::Unavailable
                },
                healthy: provider.healthy,
                capacity_available_bytes: provider.capacity_available_bytes,
            })
            .collect::<Vec<_>>();
        let existing = existing_provider_ids
            .iter()
            .filter_map(|provider_id| {
                candidates
                    .iter()
                    .find(|candidate| &candidate.provider_id == provider_id)
            })
            .map(|candidate| ExistingStorageReplica {
                provider_id: candidate.provider_id.clone(),
                pool_id: candidate.pool_id.clone(),
                storage_class: candidate.storage_class,
                failure_domain: candidate.failure_domain.clone(),
            })
            .collect::<Vec<_>>();
        self.plan_with_pools(encoded_size, &existing, &candidates)
    }

    pub fn plan_with_pools(
        &self,
        encoded_size: u64,
        existing_replicas: &[ExistingStorageReplica],
        pools: &[StoragePoolCandidate],
    ) -> StoragePlacementPlan {
        let existing_providers = existing_replicas
            .iter()
            .map(|replica| replica.provider_id.clone())
            .collect::<HashSet<_>>();
        let mut replica_counts = BTreeMap::<StorageClass, u32>::new();
        let mut domain_sets = BTreeMap::<StorageClass, HashSet<String>>::new();
        for replica in existing_replicas {
            *replica_counts.entry(replica.storage_class).or_default() += 1;
            domain_sets
                .entry(replica.storage_class)
                .or_default()
                .insert(replica.failure_domain.clone());
        }
        let mut candidates = pools.to_vec();
        candidates.sort_by(|left, right| {
            left.storage_class
                .cmp(&right.storage_class)
                .then_with(|| {
                    let left_domain_seen = domain_sets
                        .get(&left.storage_class)
                        .is_some_and(|domains| domains.contains(&left.failure_domain));
                    let right_domain_seen = domain_sets
                        .get(&right.storage_class)
                        .is_some_and(|domains| domains.contains(&right.failure_domain));
                    left_domain_seen
                        .cmp(&right_domain_seen)
                        .then_with(|| left.priority.cmp(&right.priority))
                })
                .then_with(|| left.pool_id.cmp(&right.pool_id))
                .then_with(|| left.provider_id.cmp(&right.provider_id))
        });

        let mut actions = Vec::new();
        for class in StorageClass::all() {
            let desired = self.policy.preferred_replicas(class);
            let desired_domains = self.policy.required_failure_domains(class);
            let current = replica_counts.get(&class).copied().unwrap_or_default();
            let current_domains = domain_sets.get(&class).map_or(0, HashSet::len) as u32;
            let mut projected = current;
            let mut projected_domains = current_domains;
            let mut selected_providers = existing_providers.clone();
            let class_candidates = candidates
                .iter()
                .filter(|candidate| candidate.storage_class == class)
                .collect::<Vec<_>>();
            for candidate in class_candidates {
                if (projected >= desired && projected_domains >= desired_domains)
                    || !candidate.enabled
                    || !candidate.healthy
                    || matches!(
                        candidate.status,
                        StoragePoolStatus::Disabled
                            | StoragePoolStatus::Unavailable
                            | StoragePoolStatus::NeedsCapacity
                    )
                    || existing_providers.contains(&candidate.provider_id)
                    || selected_providers.contains(&candidate.provider_id)
                    || candidate
                        .capacity_available_bytes
                        .is_some_and(|available| available < encoded_size)
                {
                    continue;
                }
                actions.push(StoragePlacementAction {
                    provider_id: candidate.provider_id.clone(),
                    tier: class,
                    pool_id: candidate.pool_id.clone(),
                    failure_domain: candidate.failure_domain.clone(),
                    priority: candidate.priority,
                    reason: format!(
                        "satisfy {} replica policy in pool {} (priority {})",
                        class.as_str(),
                        candidate.pool_id,
                        candidate.priority
                    ),
                });
                projected += 1;
                projected_domains += u32::from(
                    domain_sets
                        .get(&class)
                        .is_none_or(|domains| !domains.contains(&candidate.failure_domain)),
                );
                domain_sets
                    .entry(class)
                    .or_default()
                    .insert(candidate.failure_domain.clone());
                selected_providers.insert(candidate.provider_id.clone());
                if projected >= desired && projected_domains >= desired_domains {
                    break;
                }
            }
        }

        let planned_actions = actions.clone();
        let projected = |class: StorageClass| {
            replica_counts.get(&class).copied().unwrap_or_default()
                + planned_actions
                    .iter()
                    .filter(|action| action.tier == class)
                    .count() as u32
        };
        let projected_domains = |class: StorageClass| {
            let mut domains = existing_replicas
                .iter()
                .filter(|replica| replica.storage_class == class)
                .map(|replica| replica.failure_domain.clone())
                .collect::<HashSet<_>>();
            domains.extend(
                planned_actions
                    .iter()
                    .filter(|action| action.tier == class)
                    .map(|action| action.failure_domain.clone()),
            );
            domains.len() as u32
        };
        let existing_replicas_for =
            |class: StorageClass| replica_counts.get(&class).copied().unwrap_or_default();
        let required = |class: StorageClass| self.policy.required_replicas(class);
        let required_domains = |class: StorageClass| self.policy.required_failure_domains(class);
        let class_satisfied = |class: StorageClass| {
            projected(class) >= required(class)
                && projected_domains(class) >= required_domains(class)
        };
        let hot = StorageClass::Hot;
        let cold = StorageClass::Cold;
        let archive = StorageClass::Archive;
        let policy_satisfied = StorageClass::all().into_iter().all(class_satisfied);
        let explanation = if policy_satisfied {
            "storage policy satisfied".to_owned()
        } else {
            StorageClass::all()
                .into_iter()
                .filter(|class| !class_satisfied(*class))
                .map(|class| {
                    format!(
                        "{} {}/{} replicas and {}/{} failure domains",
                        class.as_str(),
                        projected(class),
                        required(class),
                        projected_domains(class),
                        required_domains(class)
                    )
                })
                .collect::<Vec<_>>()
                .join("; ")
        };
        StoragePlacementPlan {
            actions,
            existing_hot_replicas: existing_replicas_for(hot),
            existing_cold_replicas: existing_replicas_for(cold),
            existing_archive_replicas: existing_replicas_for(archive),
            existing_hot_failure_domains: existing_replicas
                .iter()
                .filter(|replica| replica.storage_class == hot)
                .map(|replica| replica.failure_domain.as_str())
                .collect::<HashSet<_>>()
                .len() as u32,
            existing_cold_failure_domains: existing_replicas
                .iter()
                .filter(|replica| replica.storage_class == cold)
                .map(|replica| replica.failure_domain.as_str())
                .collect::<HashSet<_>>()
                .len() as u32,
            existing_archive_failure_domains: existing_replicas
                .iter()
                .filter(|replica| replica.storage_class == archive)
                .map(|replica| replica.failure_domain.as_str())
                .collect::<HashSet<_>>()
                .len() as u32,
            projected_hot_replicas: projected(hot),
            projected_cold_replicas: projected(cold),
            projected_archive_replicas: projected(archive),
            projected_hot_failure_domains: projected_domains(hot),
            projected_cold_failure_domains: projected_domains(cold),
            projected_archive_failure_domains: projected_domains(archive),
            required_hot_replicas: required(hot),
            required_cold_replicas: required(cold),
            required_archive_replicas: required(archive),
            required_hot_failure_domains: required_domains(hot),
            required_cold_failure_domains: required_domains(cold),
            required_archive_failure_domains: required_domains(archive),
            policy_satisfied,
            explanation,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum StorageAccountStatus {
    #[serde(rename = "ACTIVE")]
    Active,
    #[serde(rename = "NEAR_FULL")]
    NearFull,
    #[serde(rename = "FULL")]
    Full,
    #[serde(rename = "UNAVAILABLE")]
    Unavailable,
    #[serde(rename = "AUTH_FAILED")]
    AuthFailed,
    #[serde(rename = "DISABLED")]
    Disabled,
    #[serde(rename = "NEEDS_REAUTH")]
    NeedsReauth,
}

impl StorageAccountStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "ACTIVE",
            Self::NearFull => "NEAR_FULL",
            Self::Full => "FULL",
            Self::Unavailable => "UNAVAILABLE",
            Self::AuthFailed => "AUTH_FAILED",
            Self::Disabled => "DISABLED",
            Self::NeedsReauth => "NEEDS_REAUTH",
        }
    }

    fn can_allocate(self) -> bool {
        matches!(self, Self::Active | Self::NearFull)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StorageAccountSnapshot {
    pub account_id: String,
    pub provider_id: String,
    pub pool_id: String,
    pub failure_domain: String,
    pub tier: StorageTier,
    pub status: StorageAccountStatus,
    pub capacity_bytes: u64,
    pub used_bytes: u64,
    pub reserved_bytes: u64,
    pub safety_margin_bytes: u64,
    pub last_capacity_check: Option<DateTime<Utc>>,
}

impl StorageAccountSnapshot {
    pub fn usable_free_bytes(&self) -> u64 {
        self.capacity_bytes
            .saturating_sub(self.used_bytes)
            .saturating_sub(self.reserved_bytes)
            .saturating_sub(self.safety_margin_bytes)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct CapacitySnapshot {
    pub capacity_bytes: u64,
    pub used_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StorageReservation {
    pub reservation_id: String,
    pub account_id: String,
    pub encoded_hash: String,
    pub bytes: u64,
    pub expires_at: DateTime<Utc>,
}

#[async_trait]
pub trait CapacityReservationStore: Send + Sync {
    async fn ensure_account(&self, account: StorageAccountSnapshot) -> Result<(), StorageError> {
        let _ = account;
        Ok(())
    }

    async fn set_account_status(
        &self,
        account_id: &str,
        status: StorageAccountStatus,
    ) -> Result<(), StorageError> {
        let _ = (account_id, status);
        Ok(())
    }

    async fn refresh_account_capacity(
        &self,
        account_id: &str,
        snapshot: CapacitySnapshot,
    ) -> Result<StorageAccountSnapshot, StorageError>;
    async fn list_accounts(
        &self,
        provider_id: &str,
    ) -> Result<Vec<StorageAccountSnapshot>, StorageError>;
    async fn reserve(
        &self,
        account_id: &str,
        encoded_hash: &str,
        bytes: u64,
        ttl: Duration,
    ) -> Result<StorageReservation, StorageError>;
    async fn commit(&self, reservation_id: &str) -> Result<(), StorageError>;
    async fn release(&self, reservation_id: &str) -> Result<(), StorageError>;
    async fn recover_expired(&self) -> Result<u64, StorageError>;
}

#[async_trait]
pub trait MegaAccountBackend: Send + Sync {
    fn account_id(&self) -> &str;
    fn remote_root(&self) -> &str;
    async fn health(&self) -> Result<(), StorageError>;
    async fn capacity(&self) -> Result<CapacitySnapshot, StorageError>;
    async fn object_size(&self, remote_path: &str) -> Result<Option<u64>, StorageError>;
    async fn upload_file(&self, local_path: &Path, remote_path: &str) -> Result<(), StorageError>;
    async fn download_file(&self, remote_path: &str, local_path: &Path)
    -> Result<(), StorageError>;
    async fn delete_object(&self, remote_path: &str) -> Result<(), StorageError>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MegaAccountConfig {
    pub account_id: String,
    pub credential_reference: String,
    #[serde(default)]
    pub command_dir: PathBuf,
    pub home_dir: PathBuf,
    pub remote_root: String,
    pub capacity_bytes: u64,
    #[serde(default)]
    pub safety_margin_bytes: u64,
    #[serde(default = "default_command_timeout")]
    pub timeout_seconds: u64,
    #[serde(default = "default_max_output")]
    pub max_output_bytes: usize,
}

fn default_command_timeout() -> u64 {
    120
}

fn default_max_output() -> usize {
    64 * 1024
}

fn is_mega_network_failure(message: &str) -> bool {
    let lowered = message.to_ascii_lowercase();
    lowered.contains("network")
        || lowered.contains("connection")
        || lowered.contains("timed out")
        || lowered.contains("timeout")
        || lowered.contains("dns")
        || lowered.contains("resolve host")
        || lowered.contains("could not connect")
        || lowered.contains("failed to connect")
        || lowered.contains("connection refused")
        || lowered.contains("network is unreachable")
        || lowered.contains("name or service not known")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MegaColdStorageConfig {
    pub provider_id: String,
    pub accounts: Vec<MegaAccountConfig>,
    pub tier: StorageTier,
    #[serde(default = "default_reservation_ttl")]
    pub reservation_ttl_seconds: u64,
    #[serde(default = "default_verify_existing")]
    pub verify_existing: bool,
}

fn default_reservation_ttl() -> u64 {
    3600
}

fn default_verify_existing() -> bool {
    true
}

impl MegaColdStorageConfig {
    pub fn validate(&self) -> Result<(), StorageError> {
        if self.provider_id.trim().is_empty() || self.accounts.is_empty() {
            return Err(StorageError::Configuration(
                "MEGA provider ID and at least one account are required".to_owned(),
            ));
        }
        if self.tier != StorageTier::Cold {
            return Err(StorageError::Configuration(
                "MEGA storage must be configured as COLD".to_owned(),
            ));
        }
        if self.reservation_ttl_seconds == 0 {
            return Err(StorageError::Configuration(
                "MEGA reservation TTL must be positive".to_owned(),
            ));
        }
        let mut ids = std::collections::HashSet::new();
        for account in &self.accounts {
            if account.account_id.trim().is_empty()
                || account.credential_reference.trim().is_empty()
                || account.remote_root.trim().is_empty()
                || !ids.insert(account.account_id.clone())
            {
                return Err(StorageError::Configuration(
                    "MEGA accounts require unique IDs, credential references, and remote roots"
                        .to_owned(),
                ));
            }
        }
        Ok(())
    }

    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let bytes = std::fs::read(path).map_err(StorageError::Io)?;
        let config: Self = serde_json::from_slice(&bytes)?;
        config.validate()?;
        Ok(config)
    }
}

#[derive(Clone)]
pub struct MegaCliAccount {
    config: Arc<MegaAccountConfig>,
}

impl std::fmt::Debug for MegaCliAccount {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MegaCliAccount")
            .field("account_id", &self.config.account_id)
            .field("credential_reference", &self.config.credential_reference)
            .field("command_dir", &self.config.command_dir)
            .field("home_dir", &self.config.home_dir)
            .field("remote_root", &self.config.remote_root)
            .finish()
    }
}

impl MegaCliAccount {
    pub fn new(config: MegaAccountConfig) -> Result<Self, StorageError> {
        if config.account_id.trim().is_empty()
            || config.credential_reference.trim().is_empty()
            || config.remote_root.trim().is_empty()
        {
            return Err(StorageError::Configuration(
                "MEGA account ID, credential reference, and remote root are required".to_owned(),
            ));
        }
        Ok(Self {
            config: Arc::new(config),
        })
    }

    pub fn config(&self) -> &MegaAccountConfig {
        &self.config
    }

    fn command_path(&self, command: &str) -> PathBuf {
        let name = format!("mega-{command}");
        if self.config.command_dir.as_os_str().is_empty() {
            PathBuf::from(name)
        } else {
            self.config.command_dir.join(name)
        }
    }

    async fn run_command(&self, command: &str, args: &[String]) -> Result<String, StorageError> {
        let mut process = Command::new(self.command_path(command));
        process
            .args(args)
            .env("HOME", &self.config.home_dir)
            .env("USERPROFILE", &self.config.home_dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = process
            .spawn()
            .map_err(|error| StorageError::Unavailable(error.to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| StorageError::Unavailable("MEGAcmd stdout unavailable".to_owned()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| StorageError::Unavailable("MEGAcmd stderr unavailable".to_owned()))?;
        let max_output = self.config.max_output_bytes;
        let stdout_task = tokio::spawn(read_limited(stdout, max_output));
        let stderr_task = tokio::spawn(read_limited(stderr, max_output));
        let status = match timeout(
            Duration::from_secs(self.config.timeout_seconds.max(1)),
            child.wait(),
        )
        .await
        {
            Ok(result) => result.map_err(|error| StorageError::Unavailable(error.to_string()))?,
            Err(_) => {
                let _ = child.kill().await;
                return Err(StorageError::NetworkUnavailable(format!(
                    "MEGAcmd {command} timed out"
                )));
            }
        };
        let stdout = stdout_task
            .await
            .map_err(|error| StorageError::Unavailable(error.to_string()))?;
        let stderr = stderr_task
            .await
            .map_err(|error| StorageError::Unavailable(error.to_string()))?;
        let output = String::from_utf8_lossy(&stdout.bytes).trim().to_owned();
        let error_output = String::from_utf8_lossy(&stderr.bytes).trim().to_owned();
        if stdout.truncated || stderr.truncated {
            return Err(StorageError::Provider(format!(
                "MEGAcmd {command} output exceeded configured limit"
            )));
        }
        if !status.success() {
            let message = if error_output.is_empty() {
                output
            } else {
                error_output
            };
            let lowered = message.to_ascii_lowercase();
            if lowered.contains("login")
                || lowered.contains("authentication")
                || lowered.contains("session")
            {
                return Err(StorageError::Authentication(
                    "MEGAcmd session authentication failed".to_owned(),
                ));
            }
            if is_mega_network_failure(&message) {
                return Err(StorageError::NetworkUnavailable(
                    "MEGAcmd could not reach the MEGA service".to_owned(),
                ));
            }
            return Err(StorageError::Unavailable(if message.is_empty() {
                format!("MEGAcmd {command} failed")
            } else {
                message
            }));
        }
        Ok(output)
    }

    fn parse_capacity(output: &str) -> Result<CapacitySnapshot, StorageError> {
        let line = output
            .lines()
            .find(|line| line.to_ascii_uppercase().contains("USED STORAGE"))
            .ok_or_else(|| {
                StorageError::Provider("could not parse MEGAcmd capacity output".to_owned())
            })?;
        let numbers = parse_size_values(line);
        if numbers.len() < 2 {
            return Err(StorageError::Provider(
                "MEGAcmd capacity output did not contain used and total bytes".to_owned(),
            ));
        }
        Ok(CapacitySnapshot {
            used_bytes: numbers[0],
            capacity_bytes: *numbers.last().unwrap_or(&0),
        })
    }

    fn parse_object_size(output: &str) -> Option<u64> {
        parse_size_values(output).into_iter().next()
    }
}

fn parse_size_values(line: &str) -> Vec<u64> {
    let tokens = line.split_whitespace().collect::<Vec<_>>();
    let mut values = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        let token = tokens[index].trim_matches(|character: char| {
            !(character.is_ascii_alphanumeric() || character == '.' || character == ',')
        });
        if let Some((number, multiplier)) = parse_attached_size(token) {
            let value = (number * multiplier).round();
            if value.is_finite() && value >= 0.0 && value <= u64::MAX as f64 {
                values.push(value as u64);
            }
        } else if !token.is_empty()
            && let Ok(number) = token.replace(',', ".").parse::<f64>()
        {
            let unit = tokens
                .get(index + 1)
                .and_then(|candidate| size_multiplier(candidate));
            let (multiplier, consumed_unit) = unit.unwrap_or((1.0, false));
            let value = (number * multiplier).round();
            if value.is_finite() && value >= 0.0 && value <= u64::MAX as f64 {
                values.push(value as u64);
            }
            index += usize::from(consumed_unit);
        }
        index += 1;
    }
    values
}

fn parse_attached_size(token: &str) -> Option<(f64, f64)> {
    let split = token
        .char_indices()
        .find(|(_, character)| character.is_ascii_alphabetic())
        .map(|(index, _)| index)?;
    let (number, unit) = token.split_at(split);
    let number = number.replace(',', ".").parse::<f64>().ok()?;
    Some((number, size_multiplier(unit)?.0))
}

fn size_multiplier(value: &str) -> Option<(f64, bool)> {
    let unit = value.trim_matches(|character: char| !character.is_ascii_alphabetic());
    let multiplier = match unit.to_ascii_uppercase().as_str() {
        "B" => 1.0,
        "KB" | "KIB" => 1024.0,
        "MB" | "MIB" => 1024.0 * 1024.0,
        "GB" | "GIB" => 1024.0 * 1024.0 * 1024.0,
        "TB" | "TIB" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        "PB" | "PIB" => 1024.0 * 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some((multiplier, true))
}

struct LimitedOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

async fn read_limited<R: AsyncRead + Unpin>(mut reader: R, max_bytes: usize) -> LimitedOutput {
    let mut bytes = Vec::with_capacity(max_bytes.min(8192));
    let mut buffer = [0_u8; 8192];
    let mut truncated = false;
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) => break,
            Ok(count) => {
                if bytes.len() < max_bytes {
                    let retained = count.min(max_bytes - bytes.len());
                    bytes.extend_from_slice(&buffer[..retained]);
                    truncated |= retained < count;
                } else {
                    truncated = true;
                }
            }
            Err(_) => {
                truncated = true;
                break;
            }
        }
    }
    LimitedOutput { bytes, truncated }
}

#[async_trait]
impl MegaAccountBackend for MegaCliAccount {
    fn account_id(&self) -> &str {
        &self.config.account_id
    }

    fn remote_root(&self) -> &str {
        &self.config.remote_root
    }

    async fn health(&self) -> Result<(), StorageError> {
        self.run_command("whoami", &[]).await.map(|_| ())
    }

    async fn capacity(&self) -> Result<CapacitySnapshot, StorageError> {
        Self::parse_capacity(&self.run_command("df", &[]).await?)
    }

    async fn object_size(&self, remote_path: &str) -> Result<Option<u64>, StorageError> {
        match self.run_command("du", &[remote_path.to_owned()]).await {
            Ok(output) => Ok(Self::parse_object_size(&output)),
            Err(StorageError::Unavailable(message))
                if message.to_ascii_lowercase().contains("not found")
                    || message.to_ascii_lowercase().contains("no such") =>
            {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    async fn upload_file(&self, local_path: &Path, remote_path: &str) -> Result<(), StorageError> {
        if let Some(parent) = Path::new(remote_path).parent() {
            self.run_command(
                "mkdir",
                &["-p".to_owned(), parent.to_string_lossy().into_owned()],
            )
            .await?;
        }
        self.run_command(
            "put",
            &[
                "-q".to_owned(),
                local_path.to_string_lossy().into_owned(),
                remote_path.to_owned(),
            ],
        )
        .await
        .map(|_| ())
    }

    async fn download_file(
        &self,
        remote_path: &str,
        local_path: &Path,
    ) -> Result<(), StorageError> {
        self.run_command(
            "get",
            &[
                "-q".to_owned(),
                remote_path.to_owned(),
                local_path.to_string_lossy().into_owned(),
            ],
        )
        .await
        .map(|_| ())
    }

    async fn delete_object(&self, remote_path: &str) -> Result<(), StorageError> {
        self.run_command("rm", &["-f".to_owned(), remote_path.to_owned()])
            .await
            .map(|_| ())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FakeMegaFailure {
    None,
    Unavailable,
    Authentication,
}

#[derive(Clone)]
pub struct FakeMegaAccount {
    account_id: String,
    remote_root: String,
    capacity_bytes: u64,
    files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    failure: Arc<Mutex<FakeMegaFailure>>,
}

impl std::fmt::Debug for FakeMegaAccount {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FakeMegaAccount")
            .field("account_id", &self.account_id)
            .field("capacity_bytes", &self.capacity_bytes)
            .finish_non_exhaustive()
    }
}

impl FakeMegaAccount {
    pub fn new(account_id: impl Into<String>, capacity_bytes: u64) -> Self {
        Self {
            account_id: account_id.into(),
            remote_root: "/launcher".to_owned(),
            capacity_bytes,
            files: Arc::new(Mutex::new(HashMap::new())),
            failure: Arc::new(Mutex::new(FakeMegaFailure::None)),
        }
    }

    pub async fn set_failure(&self, failure: FakeMegaFailure) {
        *self.failure.lock().await = failure;
    }

    pub async fn contains(&self, remote_path: &str) -> bool {
        self.files.lock().await.contains_key(remote_path)
    }

    async fn check_failure(&self) -> Result<(), StorageError> {
        match *self.failure.lock().await {
            FakeMegaFailure::None => Ok(()),
            FakeMegaFailure::Unavailable => Err(StorageError::Unavailable(self.account_id.clone())),
            FakeMegaFailure::Authentication => {
                Err(StorageError::Authentication(self.account_id.clone()))
            }
        }
    }
}

#[async_trait]
impl MegaAccountBackend for FakeMegaAccount {
    fn account_id(&self) -> &str {
        &self.account_id
    }

    fn remote_root(&self) -> &str {
        &self.remote_root
    }

    async fn health(&self) -> Result<(), StorageError> {
        self.check_failure().await
    }

    async fn capacity(&self) -> Result<CapacitySnapshot, StorageError> {
        self.check_failure().await?;
        let used_bytes = self
            .files
            .lock()
            .await
            .values()
            .map(|bytes| bytes.len() as u64)
            .sum();
        Ok(CapacitySnapshot {
            capacity_bytes: self.capacity_bytes,
            used_bytes,
        })
    }

    async fn object_size(&self, remote_path: &str) -> Result<Option<u64>, StorageError> {
        self.check_failure().await?;
        Ok(self
            .files
            .lock()
            .await
            .get(remote_path)
            .map(|bytes| bytes.len() as u64))
    }

    async fn upload_file(&self, local_path: &Path, remote_path: &str) -> Result<(), StorageError> {
        self.check_failure().await?;
        let bytes = tokio::fs::read(local_path).await?;
        let mut files = self.files.lock().await;
        let old_size = files.get(remote_path).map_or(0, |value| value.len() as u64);
        let used_bytes = files
            .values()
            .map(|value| value.len() as u64)
            .sum::<u64>()
            .saturating_sub(old_size)
            .saturating_add(bytes.len() as u64);
        if used_bytes > self.capacity_bytes {
            return Err(StorageError::NeedsCapacity {
                required_bytes: bytes.len() as u64,
                available_bytes: self.capacity_bytes.saturating_sub(used_bytes),
            });
        }
        files.insert(remote_path.to_owned(), bytes);
        Ok(())
    }

    async fn download_file(
        &self,
        remote_path: &str,
        local_path: &Path,
    ) -> Result<(), StorageError> {
        self.check_failure().await?;
        let bytes = self
            .files
            .lock()
            .await
            .get(remote_path)
            .cloned()
            .ok_or_else(|| StorageError::Unavailable("fake object missing".to_owned()))?;
        tokio::fs::write(local_path, bytes).await?;
        Ok(())
    }

    async fn delete_object(&self, remote_path: &str) -> Result<(), StorageError> {
        self.check_failure().await?;
        self.files.lock().await.remove(remote_path);
        Ok(())
    }
}

#[derive(Clone, Default)]
pub struct InMemoryCapacityReservationStore {
    state: Arc<Mutex<InMemoryCapacityState>>,
}

#[derive(Default)]
struct InMemoryCapacityState {
    accounts: HashMap<String, StorageAccountSnapshot>,
    reservations: HashMap<String, InMemoryReservation>,
    next_reservation: u64,
}

#[derive(Clone)]
struct InMemoryReservation {
    reservation: StorageReservation,
    state: InMemoryReservationState,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InMemoryReservationState {
    Held,
    Committed,
    Released,
    Expired,
}

impl InMemoryCapacityReservationStore {
    pub async fn register_account(
        &self,
        account_id: impl Into<String>,
        provider_id: impl Into<String>,
        capacity_bytes: u64,
        safety_margin_bytes: u64,
    ) {
        let account_id = account_id.into();
        let provider_id = provider_id.into();
        self.state.lock().await.accounts.insert(
            account_id.clone(),
            StorageAccountSnapshot {
                account_id,
                provider_id: provider_id.clone(),
                pool_id: provider_id,
                failure_domain: "mega".to_owned(),
                tier: StorageTier::Cold,
                status: StorageAccountStatus::Active,
                capacity_bytes,
                used_bytes: 0,
                reserved_bytes: 0,
                safety_margin_bytes,
                last_capacity_check: None,
            },
        );
    }

    pub async fn register_snapshot(&self, account: StorageAccountSnapshot) {
        self.state
            .lock()
            .await
            .accounts
            .insert(account.account_id.clone(), account);
    }

    pub async fn snapshots(&self) -> Vec<StorageAccountSnapshot> {
        self.state.lock().await.accounts.values().cloned().collect()
    }

    fn expire_locked(state: &mut InMemoryCapacityState) -> u64 {
        let now = Utc::now();
        let expired = state
            .reservations
            .values_mut()
            .filter(|reservation| {
                reservation.state == InMemoryReservationState::Held
                    && reservation.reservation.expires_at <= now
            })
            .map(|reservation| {
                reservation.state = InMemoryReservationState::Expired;
                reservation.reservation.account_id.clone()
            })
            .collect::<Vec<_>>();
        for account_id in &expired {
            if let Some(account) = state.accounts.get_mut(account_id) {
                account.reserved_bytes = state
                    .reservations
                    .values()
                    .filter(|reservation| {
                        reservation.reservation.account_id == *account_id
                            && reservation.state == InMemoryReservationState::Held
                    })
                    .map(|reservation| reservation.reservation.bytes)
                    .sum();
            }
        }
        expired.len() as u64
    }
}

#[async_trait]
impl CapacityReservationStore for InMemoryCapacityReservationStore {
    async fn ensure_account(&self, account: StorageAccountSnapshot) -> Result<(), StorageError> {
        self.register_snapshot(account).await;
        Ok(())
    }

    async fn set_account_status(
        &self,
        account_id: &str,
        status: StorageAccountStatus,
    ) -> Result<(), StorageError> {
        let mut state = self.state.lock().await;
        let account = state.accounts.get_mut(account_id).ok_or_else(|| {
            StorageError::Configuration(format!("unknown storage account {account_id}"))
        })?;
        account.status = status;
        Ok(())
    }

    async fn refresh_account_capacity(
        &self,
        account_id: &str,
        snapshot: CapacitySnapshot,
    ) -> Result<StorageAccountSnapshot, StorageError> {
        let mut state = self.state.lock().await;
        let account = state.accounts.get_mut(account_id).ok_or_else(|| {
            StorageError::Configuration(format!("unknown storage account {account_id}"))
        })?;
        account.capacity_bytes = snapshot.capacity_bytes;
        account.used_bytes = snapshot.used_bytes;
        account.last_capacity_check = Some(Utc::now());
        if account.status != StorageAccountStatus::Disabled {
            account.status = if account.usable_free_bytes() == 0 {
                StorageAccountStatus::Full
            } else if account.usable_free_bytes() <= account.safety_margin_bytes.saturating_mul(2) {
                StorageAccountStatus::NearFull
            } else {
                StorageAccountStatus::Active
            };
        }
        Ok(account.clone())
    }

    async fn list_accounts(
        &self,
        provider_id: &str,
    ) -> Result<Vec<StorageAccountSnapshot>, StorageError> {
        let mut state = self.state.lock().await;
        Self::expire_locked(&mut state);
        let mut accounts = state
            .accounts
            .values()
            .filter(|account| account.provider_id == provider_id)
            .cloned()
            .collect::<Vec<_>>();
        accounts.sort_by(|left, right| left.account_id.cmp(&right.account_id));
        Ok(accounts)
    }

    async fn reserve(
        &self,
        account_id: &str,
        encoded_hash: &str,
        bytes: u64,
        ttl: Duration,
    ) -> Result<StorageReservation, StorageError> {
        let mut state = self.state.lock().await;
        Self::expire_locked(&mut state);
        if let Some(existing) = state.reservations.values().find(|reservation| {
            reservation.reservation.account_id == account_id
                && reservation.reservation.encoded_hash == encoded_hash
                && matches!(
                    reservation.state,
                    InMemoryReservationState::Held | InMemoryReservationState::Committed
                )
        }) {
            return Ok(existing.reservation.clone());
        }
        let account = state.accounts.get(account_id).ok_or_else(|| {
            StorageError::Configuration(format!("unknown storage account {account_id}"))
        })?;
        let available = account.usable_free_bytes();
        if !account.status.can_allocate() {
            if available < bytes {
                return Err(StorageError::NeedsCapacity {
                    required_bytes: bytes,
                    available_bytes: available,
                });
            }
            return Err(StorageError::Unavailable(account_id.to_owned()));
        }
        if available < bytes {
            return Err(StorageError::NeedsCapacity {
                required_bytes: bytes,
                available_bytes: available,
            });
        }
        state.next_reservation = state.next_reservation.saturating_add(1);
        let reservation = StorageReservation {
            reservation_id: format!("memory-{}", state.next_reservation),
            account_id: account_id.to_owned(),
            encoded_hash: encoded_hash.to_owned(),
            bytes,
            expires_at: Utc::now()
                + chrono::Duration::from_std(ttl)
                    .map_err(|error| StorageError::Configuration(error.to_string()))?,
        };
        let reserved_bytes = state
            .accounts
            .get(account_id)
            .expect("account checked above")
            .reserved_bytes;
        state
            .accounts
            .get_mut(account_id)
            .expect("account checked above")
            .reserved_bytes = reserved_bytes.saturating_add(bytes);
        state.reservations.insert(
            reservation.reservation_id.clone(),
            InMemoryReservation {
                reservation: reservation.clone(),
                state: InMemoryReservationState::Held,
            },
        );
        Ok(reservation)
    }

    async fn commit(&self, reservation_id: &str) -> Result<(), StorageError> {
        let mut state = self.state.lock().await;
        let (account_id, bytes) = {
            let reservation = state.reservations.get_mut(reservation_id).ok_or_else(|| {
                StorageError::Configuration(format!("unknown reservation {reservation_id}"))
            })?;
            if reservation.state != InMemoryReservationState::Held {
                return Ok(());
            }
            reservation.state = InMemoryReservationState::Committed;
            (
                reservation.reservation.account_id.clone(),
                reservation.reservation.bytes,
            )
        };
        if let Some(account) = state.accounts.get_mut(&account_id) {
            account.reserved_bytes = account.reserved_bytes.saturating_sub(bytes);
            account.used_bytes = account.used_bytes.saturating_add(bytes);
        }
        Ok(())
    }

    async fn release(&self, reservation_id: &str) -> Result<(), StorageError> {
        let mut state = self.state.lock().await;
        let (account_id, bytes) = {
            let reservation = state.reservations.get_mut(reservation_id).ok_or_else(|| {
                StorageError::Configuration(format!("unknown reservation {reservation_id}"))
            })?;
            if reservation.state != InMemoryReservationState::Held {
                return Ok(());
            }
            reservation.state = InMemoryReservationState::Released;
            (
                reservation.reservation.account_id.clone(),
                reservation.reservation.bytes,
            )
        };
        if let Some(account) = state.accounts.get_mut(&account_id) {
            account.reserved_bytes = account.reserved_bytes.saturating_sub(bytes);
        }
        Ok(())
    }

    async fn recover_expired(&self) -> Result<u64, StorageError> {
        let mut state = self.state.lock().await;
        Ok(Self::expire_locked(&mut state))
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum StoragePoolStatus {
    #[serde(rename = "READY")]
    Ready,
    #[serde(rename = "DEGRADED")]
    Degraded,
    #[serde(rename = "NEEDS_CAPACITY")]
    NeedsCapacity,
    #[serde(rename = "UNAVAILABLE")]
    Unavailable,
    #[serde(rename = "DISABLED")]
    Disabled,
}

impl StoragePoolStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "READY",
            Self::Degraded => "DEGRADED",
            Self::NeedsCapacity => "NEEDS_CAPACITY",
            Self::Unavailable => "UNAVAILABLE",
            Self::Disabled => "DISABLED",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum ProvisioningMode {
    #[serde(rename = "DISABLED")]
    Disabled,
    #[serde(rename = "MANUAL")]
    #[default]
    Manual,
    #[serde(rename = "AUTOMATIC")]
    Automatic,
}

impl std::fmt::Display for ProvisioningMode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Disabled => "DISABLED",
            Self::Manual => "MANUAL",
            Self::Automatic => "AUTOMATIC",
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoragePool {
    pub id: String,
    pub storage_class: StorageClass,
    pub provider_type: String,
    pub priority: i32,
    pub failure_domain: String,
    pub enabled: bool,
    pub status: StoragePoolStatus,
    #[serde(default)]
    pub provisioning_mode: ProvisioningMode,
}

pub type StoragePoolMetadata = StoragePool;

impl StoragePool {
    pub fn for_provider(
        provider_id: impl Into<String>,
        storage_class: StorageClass,
        provider_type: impl Into<String>,
        failure_domain: impl Into<String>,
    ) -> Self {
        let id = provider_id.into();
        let provider_type = provider_type.into();
        Self {
            failure_domain: failure_domain.into(),
            id,
            storage_class,
            provisioning_mode: if provider_type.eq_ignore_ascii_case("mega") {
                ProvisioningMode::Manual
            } else {
                ProvisioningMode::Disabled
            },
            priority: 100,
            enabled: true,
            status: StoragePoolStatus::Ready,
            provider_type,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProvisionedCapacity {
    pub pool_id: String,
    pub account_id: String,
    pub credential_reference: String,
    pub capacity_bytes: u64,
    pub safety_margin_bytes: u64,
}

#[async_trait]
pub trait StorageCapacityProvisioner: Send + Sync {
    fn pool_id(&self) -> &str;
    fn mode(&self) -> ProvisioningMode;
    async fn can_provision(&self, required_bytes: u64) -> Result<bool, StorageError>;
    async fn provision(&self, required_bytes: u64) -> Result<ProvisionedCapacity, StorageError>;
}

#[derive(Debug, Clone)]
pub struct ManualStorageCapacityProvisioner {
    pool_id: String,
}

impl ManualStorageCapacityProvisioner {
    pub fn new(pool_id: impl Into<String>) -> Self {
        Self {
            pool_id: pool_id.into(),
        }
    }
}

#[async_trait]
impl StorageCapacityProvisioner for ManualStorageCapacityProvisioner {
    fn pool_id(&self) -> &str {
        &self.pool_id
    }

    fn mode(&self) -> ProvisioningMode {
        ProvisioningMode::Manual
    }

    async fn can_provision(&self, _required_bytes: u64) -> Result<bool, StorageError> {
        Ok(false)
    }

    async fn provision(&self, required_bytes: u64) -> Result<ProvisionedCapacity, StorageError> {
        Err(StorageError::NeedsCapacity {
            required_bytes,
            available_bytes: 0,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CapacityPoolSnapshot {
    pub pool: StoragePool,
    pub available_bytes: u64,
    pub reserved_bytes: u64,
    pub healthy: bool,
}

#[derive(Debug, Clone, Default)]
pub struct StorageCapacityManager;

impl StorageCapacityManager {
    pub fn select_pool(
        required_bytes: u64,
        pools: &[CapacityPoolSnapshot],
    ) -> Result<String, StorageError> {
        let mut candidates = pools
            .iter()
            .filter(|pool| {
                pool.pool.enabled
                    && pool.healthy
                    && matches!(
                        pool.pool.status,
                        StoragePoolStatus::Ready | StoragePoolStatus::Degraded
                    )
                    && pool.available_bytes >= required_bytes
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            left.pool
                .priority
                .cmp(&right.pool.priority)
                .then_with(|| left.pool.id.cmp(&right.pool.id))
        });
        candidates
            .first()
            .map(|pool| pool.pool.id.clone())
            .ok_or(StorageError::NeedsCapacity {
                required_bytes,
                available_bytes: pools
                    .iter()
                    .map(|pool| pool.available_bytes)
                    .max()
                    .unwrap_or_default(),
            })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoragePoolHealth {
    pub pool_id: String,
    pub provider_id: String,
    pub storage_class: StorageClass,
    pub provider_type: String,
    pub priority: i32,
    pub failure_domain: String,
    pub enabled: bool,
    pub provisioning_mode: ProvisioningMode,
    pub status: StoragePoolStatus,
    pub total_capacity_bytes: u64,
    pub total_used_bytes: u64,
    pub total_reserved_bytes: u64,
    pub available_bytes: u64,
    pub accounts: Vec<StorageAccountSnapshot>,
}

#[derive(Debug, Clone)]
pub struct MegaColdStorageOptions {
    pub safety_margin_bytes: u64,
    pub reservation_ttl: Duration,
    pub verify_existing: bool,
}

impl Default for MegaColdStorageOptions {
    fn default() -> Self {
        Self {
            safety_margin_bytes: 0,
            reservation_ttl: Duration::from_secs(3600),
            verify_existing: true,
        }
    }
}

#[derive(Clone)]
pub struct MegaColdStoragePool {
    provider_id: String,
    pool: StoragePool,
    accounts: Arc<Vec<Arc<dyn MegaAccountBackend>>>,
    ledger: Arc<dyn CapacityReservationStore>,
    options: Arc<MegaColdStorageOptions>,
    provisioner: Arc<dyn StorageCapacityProvisioner>,
    temp_counter: Arc<AtomicU64>,
}

impl std::fmt::Debug for MegaColdStoragePool {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MegaColdStoragePool")
            .field("provider_id", &self.provider_id)
            .field(
                "accounts",
                &self
                    .accounts
                    .iter()
                    .map(|account| account.account_id())
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl MegaColdStoragePool {
    pub fn new(
        provider_id: impl Into<String>,
        accounts: Vec<Arc<dyn MegaAccountBackend>>,
        ledger: Arc<dyn CapacityReservationStore>,
        options: MegaColdStorageOptions,
    ) -> Result<Self, StorageError> {
        Self::new_with_provisioner(provider_id, accounts, ledger, options, None)
    }

    pub fn new_with_provisioner(
        provider_id: impl Into<String>,
        mut accounts: Vec<Arc<dyn MegaAccountBackend>>,
        ledger: Arc<dyn CapacityReservationStore>,
        options: MegaColdStorageOptions,
        provisioner: Option<Arc<dyn StorageCapacityProvisioner>>,
    ) -> Result<Self, StorageError> {
        let provider_id = provider_id.into();
        if provider_id.trim().is_empty() || accounts.is_empty() {
            return Err(StorageError::Configuration(
                "MEGA cold pool requires a provider ID and at least one account".to_owned(),
            ));
        }
        accounts.sort_by(|left, right| left.account_id().cmp(right.account_id()));
        let provisioner = provisioner.unwrap_or_else(|| {
            Arc::new(ManualStorageCapacityProvisioner::new(provider_id.clone()))
                as Arc<dyn StorageCapacityProvisioner>
        });
        let pool = StoragePool {
            id: provider_id.clone(),
            storage_class: StorageClass::Cold,
            provider_type: "mega".to_owned(),
            priority: 100,
            failure_domain: "mega".to_owned(),
            enabled: true,
            status: StoragePoolStatus::Ready,
            provisioning_mode: provisioner.mode(),
        };
        Ok(Self {
            provider_id: provider_id.clone(),
            pool,
            accounts: Arc::new(accounts),
            ledger,
            options: Arc::new(options),
            provisioner,
            temp_counter: Arc::new(AtomicU64::new(0)),
        })
    }

    pub fn from_config(
        config: MegaColdStorageConfig,
        ledger: Arc<dyn CapacityReservationStore>,
    ) -> Result<Self, StorageError> {
        config.validate()?;
        let accounts = config
            .accounts
            .iter()
            .cloned()
            .map(MegaCliAccount::new)
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(|account| Arc::new(account) as Arc<dyn MegaAccountBackend>)
            .collect();
        Self::new(
            config.provider_id,
            accounts,
            ledger,
            MegaColdStorageOptions {
                safety_margin_bytes: config
                    .accounts
                    .iter()
                    .map(|account| account.safety_margin_bytes)
                    .max()
                    .unwrap_or_default(),
                reservation_ttl: Duration::from_secs(config.reservation_ttl_seconds),
                verify_existing: config.verify_existing,
            },
        )
    }

    pub async fn from_config_and_register(
        config: MegaColdStorageConfig,
        ledger: Arc<dyn CapacityReservationStore>,
    ) -> Result<Self, StorageError> {
        config.validate()?;
        for account in &config.accounts {
            ledger
                .ensure_account(StorageAccountSnapshot {
                    account_id: account.account_id.clone(),
                    provider_id: config.provider_id.clone(),
                    pool_id: config.provider_id.clone(),
                    failure_domain: "mega".to_owned(),
                    tier: StorageTier::Cold,
                    status: StorageAccountStatus::Active,
                    capacity_bytes: account.capacity_bytes,
                    used_bytes: 0,
                    reserved_bytes: 0,
                    safety_margin_bytes: account.safety_margin_bytes,
                    last_capacity_check: None,
                })
                .await?;
        }
        Self::from_config(config, ledger)
    }

    pub fn provider_id(&self) -> &str {
        &self.provider_id
    }

    pub fn pool(&self) -> &StoragePool {
        &self.pool
    }

    pub fn provisioner(&self) -> &dyn StorageCapacityProvisioner {
        self.provisioner.as_ref()
    }

    pub async fn provision_capacity(
        &self,
        required_bytes: u64,
    ) -> Result<ProvisionedCapacity, StorageError> {
        if !self.provisioner.can_provision(required_bytes).await? {
            return Err(StorageError::NeedsCapacity {
                required_bytes,
                available_bytes: 0,
            });
        }
        let provisioned = self.provisioner.provision(required_bytes).await?;
        if provisioned.pool_id != self.pool.id {
            return Err(StorageError::Configuration(format!(
                "capacity provisioner returned pool {} for {}",
                provisioned.pool_id, self.pool.id
            )));
        }
        Ok(provisioned)
    }

    pub fn account_ids(&self) -> Vec<String> {
        self.accounts
            .iter()
            .map(|account| account.account_id().to_owned())
            .collect()
    }

    fn object_path(account: &dyn MegaAccountBackend, encoded_hash: &str) -> String {
        let root = account.remote_root().trim_end_matches('/');
        format!(
            "{root}/chunks/{}/{}/{}.chunk",
            &encoded_hash[0..2],
            &encoded_hash[2..4],
            encoded_hash
        )
    }

    fn temp_path(&self, encoded_hash: &str, suffix: &str) -> PathBuf {
        let sequence = self.temp_counter.fetch_add(1, Ordering::AcqRel);
        std::env::temp_dir().join(format!(
            "launcher-mega-{encoded_hash}-{sequence}-{suffix}.part"
        ))
    }

    async fn verify_existing(
        &self,
        account: &dyn MegaAccountBackend,
        remote_path: &str,
        encoded_hash: &str,
        expected_size: usize,
    ) -> Result<bool, StorageError> {
        let Some(size) = account.object_size(remote_path).await? else {
            return Ok(false);
        };
        if size != expected_size as u64 || !self.options.verify_existing {
            return Ok(size == expected_size as u64 && !self.options.verify_existing);
        }
        let temporary = self.temp_path(encoded_hash, "verify");
        let result = async {
            account.download_file(remote_path, &temporary).await?;
            let bytes = tokio::fs::read(&temporary).await?;
            Ok::<bool, StorageError>(verify_encoded_bytes(encoded_hash, &bytes).is_ok())
        }
        .await;
        let _ = tokio::fs::remove_file(&temporary).await;
        result
    }

    pub async fn health(&self) -> StoragePoolHealth {
        let mut accounts = self
            .ledger
            .list_accounts(&self.provider_id)
            .await
            .unwrap_or_default();
        let mut healthy = 0;
        let mut auth_failed = false;
        let mut network_unavailable = false;
        for account in self.accounts.iter() {
            match account.health().await {
                Ok(()) => match account.capacity().await {
                    Ok(capacity) => {
                        healthy += 1;
                        let _ = self
                            .ledger
                            .refresh_account_capacity(account.account_id(), capacity)
                            .await;
                    }
                    Err(StorageError::Authentication(_)) => {
                        auth_failed = true;
                        let _ = self
                            .ledger
                            .set_account_status(
                                account.account_id(),
                                StorageAccountStatus::AuthFailed,
                            )
                            .await;
                    }
                    Err(StorageError::NetworkUnavailable(_)) => {
                        network_unavailable = true;
                        let _ = self
                            .ledger
                            .set_account_status(
                                account.account_id(),
                                StorageAccountStatus::Unavailable,
                            )
                            .await;
                    }
                    Err(_) => {
                        let _ = self
                            .ledger
                            .set_account_status(
                                account.account_id(),
                                StorageAccountStatus::Unavailable,
                            )
                            .await;
                    }
                },
                Err(StorageError::Authentication(_)) => {
                    auth_failed = true;
                    let _ = self
                        .ledger
                        .set_account_status(account.account_id(), StorageAccountStatus::AuthFailed)
                        .await;
                }
                Err(StorageError::NetworkUnavailable(_)) => {
                    network_unavailable = true;
                    let _ = self
                        .ledger
                        .set_account_status(account.account_id(), StorageAccountStatus::Unavailable)
                        .await;
                }
                Err(_) => {
                    let _ = self
                        .ledger
                        .set_account_status(account.account_id(), StorageAccountStatus::Unavailable)
                        .await;
                }
            }
        }
        accounts = self
            .ledger
            .list_accounts(&self.provider_id)
            .await
            .unwrap_or(accounts);
        let total_capacity_bytes = accounts.iter().map(|account| account.capacity_bytes).sum();
        let total_used_bytes = accounts.iter().map(|account| account.used_bytes).sum();
        let total_reserved_bytes = accounts.iter().map(|account| account.reserved_bytes).sum();
        let available_bytes = accounts
            .iter()
            .map(StorageAccountSnapshot::usable_free_bytes)
            .sum();
        let active = accounts
            .iter()
            .filter(|account| account.status.can_allocate())
            .count();
        let status = if !self.pool.enabled {
            StoragePoolStatus::Disabled
        } else if active == 0 && (auth_failed || network_unavailable) {
            StoragePoolStatus::Unavailable
        } else if active == 0 || available_bytes == 0 {
            StoragePoolStatus::NeedsCapacity
        } else if healthy < self.accounts.len() {
            StoragePoolStatus::Degraded
        } else {
            StoragePoolStatus::Ready
        };
        StoragePoolHealth {
            pool_id: self.pool.id.clone(),
            provider_id: self.provider_id.clone(),
            storage_class: self.pool.storage_class,
            provider_type: self.pool.provider_type.clone(),
            priority: self.pool.priority,
            failure_domain: self.pool.failure_domain.clone(),
            enabled: self.pool.enabled,
            provisioning_mode: self.provisioner.mode(),
            status,
            total_capacity_bytes,
            total_used_bytes,
            total_reserved_bytes,
            available_bytes,
            accounts,
        }
    }

    async fn put_inner(&self, encoded_hash: &str, bytes: &[u8]) -> Result<(), StorageError> {
        let temporary = self.temp_path(encoded_hash, "upload");
        tokio::fs::write(&temporary, bytes).await?;
        let mut capacity_available: u64 = 0;
        let mut capacity_checked = false;
        let mut last_error = None;
        for account in self.accounts.iter() {
            let remote_path = Self::object_path(account.as_ref(), encoded_hash);
            match self
                .verify_existing(account.as_ref(), &remote_path, encoded_hash, bytes.len())
                .await
            {
                Ok(true) => {
                    let _ = tokio::fs::remove_file(&temporary).await;
                    return Ok(());
                }
                Ok(false) => {}
                Err(error) => {
                    last_error = Some(error);
                    continue;
                }
            }
            if let Ok(Some(_)) = account.object_size(&remote_path).await
                && let Err(error) = account.delete_object(&remote_path).await
            {
                last_error = Some(error);
                continue;
            }
            let capacity = match account.capacity().await {
                Ok(capacity) => capacity,
                Err(error) => {
                    last_error = Some(error);
                    continue;
                }
            };
            capacity_checked = true;
            let snapshot = match self
                .ledger
                .refresh_account_capacity(account.account_id(), capacity)
                .await
            {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    last_error = Some(error);
                    continue;
                }
            };
            capacity_available = capacity_available.saturating_add(snapshot.usable_free_bytes());
            let reservation = match self
                .ledger
                .reserve(
                    account.account_id(),
                    encoded_hash,
                    bytes.len() as u64,
                    self.options.reservation_ttl,
                )
                .await
            {
                Ok(reservation) => reservation,
                Err(StorageError::NeedsCapacity { .. }) => continue,
                Err(error) => {
                    last_error = Some(error);
                    continue;
                }
            };
            let upload_result = async {
                account.upload_file(&temporary, &remote_path).await?;
                let size = account.object_size(&remote_path).await?;
                if size != Some(bytes.len() as u64) {
                    return Err(StorageError::Provider(
                        "MEGA upload size verification failed".to_owned(),
                    ));
                }
                self.ledger.commit(&reservation.reservation_id).await
            }
            .await;
            match upload_result {
                Ok(()) => {
                    let _ = tokio::fs::remove_file(&temporary).await;
                    return Ok(());
                }
                Err(error) => {
                    let _ = self.ledger.release(&reservation.reservation_id).await;
                    let _ = account.delete_object(&remote_path).await;
                    last_error = Some(error);
                }
            }
        }
        let _ = tokio::fs::remove_file(&temporary).await;
        if let Some(StorageError::Authentication(message)) = last_error {
            return Err(StorageError::Authentication(message));
        }
        if capacity_available == 0 {
            if let Some(error) = last_error {
                return Err(error);
            }
            if capacity_checked {
                return Err(StorageError::NeedsCapacity {
                    required_bytes: bytes.len() as u64,
                    available_bytes: 0,
                });
            }
            return Err(StorageError::PoolUnavailable);
        }
        Err(StorageError::NeedsCapacity {
            required_bytes: bytes.len() as u64,
            available_bytes: capacity_available,
        })
    }
}

#[async_trait]
impl StorageProvider for MegaColdStoragePool {
    fn provider_id(&self) -> &str {
        &self.provider_id
    }

    fn tier(&self) -> StorageTier {
        StorageTier::Cold
    }

    fn pool_id(&self) -> &str {
        &self.pool.id
    }

    fn provider_type(&self) -> &str {
        &self.pool.provider_type
    }

    fn failure_domain(&self) -> &str {
        &self.pool.failure_domain
    }

    async fn put_encoded(&self, encoded_hash: &str, bytes: &[u8]) -> Result<(), StorageError> {
        super::validate_hash(encoded_hash)?;
        verify_encoded_bytes(encoded_hash, bytes)?;
        self.put_inner(encoded_hash, bytes).await
    }

    async fn read_encoded(&self, encoded_hash: &str) -> Result<Vec<u8>, StorageError> {
        super::validate_hash(encoded_hash)?;
        let mut last_error = None;
        for account in self.accounts.iter() {
            let remote_path = Self::object_path(account.as_ref(), encoded_hash);
            let size = match account.object_size(&remote_path).await {
                Ok(Some(size)) => size,
                Ok(None) => continue,
                Err(error) => {
                    last_error = Some(error);
                    continue;
                }
            };
            let temporary = self.temp_path(encoded_hash, "download");
            let result = async {
                account.download_file(&remote_path, &temporary).await?;
                let bytes = tokio::fs::read(&temporary).await?;
                if size != bytes.len() as u64 {
                    return Err(StorageError::Provider(
                        "MEGA download size verification failed".to_owned(),
                    ));
                }
                verify_encoded_bytes(encoded_hash, &bytes)?;
                Ok(bytes)
            }
            .await;
            let _ = tokio::fs::remove_file(&temporary).await;
            match result {
                Ok(bytes) => return Ok(bytes),
                Err(error) => {
                    last_error = Some(error);
                    continue;
                }
            }
        }
        if let Some(error) = last_error {
            return Err(error);
        }
        Err(StorageError::Unavailable(format!(
            "cold object {encoded_hash} is unavailable"
        )))
    }

    async fn head_encoded(&self, encoded_hash: &str) -> Result<Option<u64>, StorageError> {
        super::validate_hash(encoded_hash)?;
        let mut last_error = None;
        for account in self.accounts.iter() {
            let remote_path = Self::object_path(account.as_ref(), encoded_hash);
            match account.object_size(&remote_path).await {
                Ok(Some(size)) => return Ok(Some(size)),
                Ok(None) => {}
                Err(error) => last_error = Some(error),
            }
        }
        if let Some(error) = last_error {
            return Err(error);
        }
        Ok(None)
    }

    async fn delete_encoded(&self, encoded_hash: &str) -> Result<(), StorageError> {
        super::validate_hash(encoded_hash)?;
        let mut last_error = None;
        for account in self.accounts.iter() {
            let remote_path = Self::object_path(account.as_ref(), encoded_hash);
            match account.object_size(&remote_path).await {
                Ok(Some(_)) => {
                    if let Err(error) = account.delete_object(&remote_path).await {
                        last_error = Some(error);
                    }
                }
                Ok(None) => {}
                Err(error) => last_error = Some(error),
            }
        }
        if let Some(error) = last_error {
            return Err(error);
        }
        Ok(())
    }

    async fn download_location(
        &self,
        _encoded_hash: &str,
    ) -> Result<DownloadLocation, StorageError> {
        Err(StorageError::Provider(
            "cold storage is not client-facing".to_owned(),
        ))
    }

    async fn health_check(&self) -> Result<(), StorageError> {
        let health = self.health().await;
        match health.status {
            StoragePoolStatus::Ready => Ok(()),
            StoragePoolStatus::Degraded => Err(StorageError::Unavailable(
                "MEGA cold pool is degraded".to_owned(),
            )),
            StoragePoolStatus::NeedsCapacity => Err(StorageError::NeedsCapacity {
                required_bytes: 1,
                available_bytes: health.available_bytes,
            }),
            StoragePoolStatus::Unavailable | StoragePoolStatus::Disabled => {
                Err(StorageError::PoolUnavailable)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(
        provider_id: &str,
        tier: StorageTier,
        healthy: bool,
        capacity_available_bytes: Option<u64>,
    ) -> PlacementProvider {
        PlacementProvider {
            provider_id: provider_id.to_owned(),
            tier,
            healthy,
            capacity_available_bytes,
        }
    }

    #[test]
    fn placement_requires_hot_and_cold_replicas_and_skips_unsafe_candidates() {
        let engine = StoragePlacementEngine::new(StoragePolicy::staging()).unwrap();
        let plan = engine.plan(
            100,
            &[],
            &[
                provider("cold-full", StorageTier::Cold, true, Some(10)),
                provider("cold-a", StorageTier::Cold, true, Some(100)),
                provider("hot-a", StorageTier::Hot, true, Some(100)),
                provider("hot-down", StorageTier::Hot, false, Some(100)),
            ],
        );
        assert!(plan.policy_satisfied);
        assert_eq!(plan.actions.len(), 2);
        assert!(
            plan.actions
                .iter()
                .any(|action| action.provider_id == "hot-a")
        );
        assert!(
            plan.actions
                .iter()
                .any(|action| action.provider_id == "cold-a")
        );
        assert!(
            !plan
                .actions
                .iter()
                .any(|action| action.provider_id == "cold-full")
        );
    }

    #[test]
    fn parses_all_storage_classes() {
        assert_eq!("HOT".parse::<StorageClass>().unwrap(), StorageClass::Hot);
        assert_eq!("cold".parse::<StorageClass>().unwrap(), StorageClass::Cold);
        assert_eq!(
            "archive".parse::<StorageClass>().unwrap(),
            StorageClass::Archive
        );
    }

    #[test]
    fn placement_prefers_enabled_healthy_pool_priority_and_skips_full_pools() {
        let policy = StoragePolicy {
            min_verified_cold_replicas: 1,
            preferred_cold_replicas: 1,
            min_cold_failure_domains: 1,
            ..StoragePolicy::default()
        };
        let engine = StoragePlacementEngine::new(policy).unwrap();
        let plan = engine.plan_with_pools(
            100,
            &[ExistingStorageReplica {
                provider_id: "hot-provider".to_owned(),
                pool_id: "hot-primary".to_owned(),
                storage_class: StorageClass::Hot,
                failure_domain: "railway".to_owned(),
            }],
            &[
                StoragePoolCandidate {
                    capacity_available_bytes: Some(10),
                    ..StoragePoolCandidate::new(
                        "cold-full",
                        "cold-full-provider",
                        StorageClass::Cold,
                        "mega",
                        10,
                        "mega",
                    )
                },
                StoragePoolCandidate {
                    capacity_available_bytes: Some(10),
                    ..StoragePoolCandidate::new(
                        "cold-too-small",
                        "cold-too-small-provider",
                        StorageClass::Cold,
                        "mega",
                        20,
                        "mega",
                    )
                },
                StoragePoolCandidate::new(
                    "cold-primary",
                    "cold-primary-provider",
                    StorageClass::Cold,
                    "mega",
                    100,
                    "mega",
                ),
                StoragePoolCandidate::new(
                    "hot-primary",
                    "hot-provider",
                    StorageClass::Hot,
                    "s3",
                    100,
                    "railway",
                ),
            ],
        );
        assert!(plan.policy_satisfied);
        assert_eq!(plan.actions.len(), 1);
        assert_eq!(plan.actions[0].pool_id, "cold-primary");
        assert_eq!(plan.actions[0].priority, 100);
    }

    #[test]
    fn failure_domain_requirement_distinguishes_same_pool_replicas() {
        let policy = StoragePolicy {
            min_verified_cold_replicas: 2,
            preferred_cold_replicas: 2,
            min_cold_failure_domains: 2,
            ..StoragePolicy::default()
        };
        let engine = StoragePlacementEngine::new(policy).unwrap();
        let existing = [
            ExistingStorageReplica {
                provider_id: "hot".to_owned(),
                pool_id: "hot".to_owned(),
                storage_class: StorageClass::Hot,
                failure_domain: "railway".to_owned(),
            },
            ExistingStorageReplica {
                provider_id: "mega-a".to_owned(),
                pool_id: "mega".to_owned(),
                storage_class: StorageClass::Cold,
                failure_domain: "mega".to_owned(),
            },
            ExistingStorageReplica {
                provider_id: "mega-b".to_owned(),
                pool_id: "mega".to_owned(),
                storage_class: StorageClass::Cold,
                failure_domain: "mega".to_owned(),
            },
        ];
        let plan = engine.plan_with_pools(
            10,
            &existing,
            &[
                StoragePoolCandidate::new("hot", "hot", StorageClass::Hot, "s3", 100, "railway"),
                StoragePoolCandidate::new(
                    "mega",
                    "mega-c",
                    StorageClass::Cold,
                    "mega",
                    100,
                    "mega",
                ),
                StoragePoolCandidate::new(
                    "archive-fallback",
                    "archive-fallback",
                    StorageClass::Cold,
                    "s3",
                    200,
                    "archive-provider",
                ),
            ],
        );
        assert!(plan.policy_satisfied);
        assert_eq!(plan.existing_cold_replicas, 2);
        assert_eq!(plan.existing_cold_failure_domains, 1);
        assert_eq!(plan.projected_cold_failure_domains, 2);
        assert_eq!(plan.actions[0].pool_id, "archive-fallback");
    }

    #[test]
    fn capacity_manager_skips_disabled_unhealthy_and_full_pools() {
        let make_pool = |id: &str, priority: i32| StoragePool {
            id: id.to_owned(),
            storage_class: StorageClass::Cold,
            provider_type: "test".to_owned(),
            priority,
            failure_domain: id.to_owned(),
            enabled: true,
            status: StoragePoolStatus::Ready,
            provisioning_mode: ProvisioningMode::Manual,
        };
        let mut disabled = make_pool("disabled", 1);
        disabled.enabled = false;
        let mut unhealthy = make_pool("unhealthy", 2);
        unhealthy.status = StoragePoolStatus::Unavailable;
        let full = make_pool("full", 3);
        let fallback = make_pool("fallback", 4);
        let selected = StorageCapacityManager::select_pool(
            100,
            &[
                CapacityPoolSnapshot {
                    pool: disabled,
                    available_bytes: 1000,
                    reserved_bytes: 0,
                    healthy: true,
                },
                CapacityPoolSnapshot {
                    pool: unhealthy,
                    available_bytes: 1000,
                    reserved_bytes: 0,
                    healthy: true,
                },
                CapacityPoolSnapshot {
                    pool: full,
                    available_bytes: 10,
                    reserved_bytes: 0,
                    healthy: true,
                },
                CapacityPoolSnapshot {
                    pool: fallback,
                    available_bytes: 1000,
                    reserved_bytes: 0,
                    healthy: true,
                },
            ],
        )
        .unwrap();
        assert_eq!(selected, "fallback");
    }

    struct AutomaticProvisioner {
        pool_id: String,
    }

    #[async_trait]
    impl StorageCapacityProvisioner for AutomaticProvisioner {
        fn pool_id(&self) -> &str {
            &self.pool_id
        }

        fn mode(&self) -> ProvisioningMode {
            ProvisioningMode::Automatic
        }

        async fn can_provision(&self, required_bytes: u64) -> Result<bool, StorageError> {
            Ok(required_bytes <= 1024)
        }

        async fn provision(
            &self,
            required_bytes: u64,
        ) -> Result<ProvisionedCapacity, StorageError> {
            Ok(ProvisionedCapacity {
                pool_id: self.pool_id.clone(),
                account_id: "auto-account".to_owned(),
                credential_reference: "secret://auto/session".to_owned(),
                capacity_bytes: required_bytes,
                safety_margin_bytes: 0,
            })
        }
    }

    #[tokio::test]
    async fn manual_provisioning_reports_needs_capacity_and_automatic_hook_returns_capacity() {
        let ledger = InMemoryCapacityReservationStore::default();
        ledger.register_account("a", "mega", 1, 1).await;
        let account = Arc::new(FakeMegaAccount::new("a", 1));
        let manual = MegaColdStoragePool::new(
            "mega",
            vec![account.clone()],
            Arc::new(ledger.clone()),
            MegaColdStorageOptions::default(),
        )
        .unwrap();
        assert!(matches!(
            manual.provision_capacity(100).await,
            Err(StorageError::NeedsCapacity { .. })
        ));
        let automatic = MegaColdStoragePool::new_with_provisioner(
            "mega",
            vec![account],
            Arc::new(ledger),
            MegaColdStorageOptions::default(),
            Some(Arc::new(AutomaticProvisioner {
                pool_id: "mega".to_owned(),
            })),
        )
        .unwrap();
        let provisioned = automatic.provision_capacity(100).await.unwrap();
        assert_eq!(provisioned.account_id, "auto-account");
        assert_eq!(automatic.provisioner().mode(), ProvisioningMode::Automatic);
    }

    #[test]
    fn parses_megacmd_human_capacity_and_object_sizes() {
        assert_eq!(
            parse_size_values("USED STORAGE: 1.5 GB OF 20 GB"),
            vec![1_610_612_736, 21_474_836_480]
        );
        assert_eq!(
            parse_size_values("123 B /launcher/chunks/object"),
            vec![123]
        );
        assert_eq!(parse_size_values("123MB"), vec![128_974_848]);
    }

    #[test]
    fn classifies_mega_network_failures_without_mislabeling_authentication() {
        assert!(is_mega_network_failure("connection refused"));
        assert!(is_mega_network_failure("DNS resolution failed"));
        assert!(!is_mega_network_failure("authentication failed"));
    }

    #[tokio::test]
    async fn pool_rolls_over_to_the_next_account_and_preserves_deterministic_layout() {
        let ledger = InMemoryCapacityReservationStore::default();
        ledger.register_account("a", "mega", 8, 0).await;
        ledger.register_account("b", "mega", 8, 0).await;
        let first = Arc::new(FakeMegaAccount::new("a", 8));
        let second = Arc::new(FakeMegaAccount::new("b", 8));
        let pool = MegaColdStoragePool::new(
            "mega",
            vec![first.clone(), second.clone()],
            Arc::new(ledger),
            MegaColdStorageOptions::default(),
        )
        .unwrap();
        let first_data = b"12345678";
        let first_hash = blake3::hash(first_data).to_hex().to_string();
        pool.put_encoded(&first_hash, first_data).await.unwrap();
        let second_data = b"abcdefgh";
        let second_hash = blake3::hash(second_data).to_hex().to_string();
        pool.put_encoded(&second_hash, second_data).await.unwrap();
        assert!(
            first
                .contains(&MegaColdStoragePool::object_path(
                    first.as_ref(),
                    &first_hash
                ))
                .await
        );
        assert!(
            second
                .contains(&MegaColdStoragePool::object_path(
                    second.as_ref(),
                    &second_hash
                ))
                .await
        );
        assert_eq!(pool.read_encoded(&second_hash).await.unwrap(), second_data);
    }

    #[tokio::test]
    async fn no_capacity_is_typed_and_pool_health_reports_capacity_failure() {
        let ledger = InMemoryCapacityReservationStore::default();
        ledger.register_account("full", "mega", 1, 1).await;
        let account = Arc::new(FakeMegaAccount::new("full", 1));
        let pool = MegaColdStoragePool::new(
            "mega",
            vec![account],
            Arc::new(ledger),
            MegaColdStorageOptions::default(),
        )
        .unwrap();
        let data = b"too large";
        let hash = blake3::hash(data).to_hex().to_string();
        assert!(matches!(
            pool.put_encoded(&hash, data).await,
            Err(StorageError::NeedsCapacity { .. })
        ));
        assert_eq!(pool.health().await.status, StoragePoolStatus::NeedsCapacity);
    }

    #[tokio::test]
    async fn reservations_are_serialized_and_released() {
        let ledger = InMemoryCapacityReservationStore::default();
        ledger.register_account("a", "mega", 10, 0).await;
        let first_hash = "a".repeat(64);
        let second_hash = "b".repeat(64);
        let first = ledger.reserve("a", &first_hash, 10, Duration::from_secs(60));
        let second = ledger.reserve("a", &second_hash, 1, Duration::from_secs(60));
        let (first, second) = tokio::join!(first, second);
        assert!(first.is_ok());
        assert!(matches!(second, Err(StorageError::NeedsCapacity { .. })));
        ledger
            .release(&first.unwrap().reservation_id)
            .await
            .unwrap();
        assert!(
            ledger
                .reserve("a", &"c".repeat(64), 10, Duration::from_secs(60))
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn duplicate_upload_is_idempotent_and_auth_failures_are_visible() {
        let ledger = InMemoryCapacityReservationStore::default();
        ledger.register_account("a", "mega", 100, 0).await;
        let account = Arc::new(FakeMegaAccount::new("a", 100));
        let pool = MegaColdStoragePool::new(
            "mega",
            vec![account.clone()],
            Arc::new(ledger),
            MegaColdStorageOptions::default(),
        )
        .unwrap();
        let data = b"idempotent";
        let hash = blake3::hash(data).to_hex().to_string();
        pool.put_encoded(&hash, data).await.unwrap();
        pool.put_encoded(&hash, data).await.unwrap();
        account.set_failure(FakeMegaFailure::Authentication).await;
        assert!(matches!(
            pool.read_encoded(&"f".repeat(64)).await,
            Err(StorageError::Authentication(_))
        ));
    }
}
