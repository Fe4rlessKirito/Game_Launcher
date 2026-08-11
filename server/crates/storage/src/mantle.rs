use super::StorageError;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;
use tokio::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MantleCacheConfig {
    pub root: PathBuf,
    pub preferred_bytes: u64,
    pub minimum_bytes: u64,
    pub minimum_free_disk_bytes: u64,
    pub emergency_free_disk_bytes: u64,
    pub lease_ttl_seconds: u64,
}

impl Default for MantleCacheConfig {
    fn default() -> Self {
        Self {
            root: PathBuf::from("mantle-cache"),
            preferred_bytes: 100 * 1024 * 1024 * 1024,
            minimum_bytes: 10 * 1024 * 1024 * 1024,
            minimum_free_disk_bytes: 10 * 1024 * 1024 * 1024,
            emergency_free_disk_bytes: 2 * 1024 * 1024 * 1024,
            lease_ttl_seconds: 3600,
        }
    }
}

impl MantleCacheConfig {
    pub fn validate(&self) -> Result<(), StorageError> {
        if self.minimum_bytes == 0
            || self.preferred_bytes < self.minimum_bytes
            || self.emergency_free_disk_bytes > self.minimum_free_disk_bytes
            || self.lease_ttl_seconds == 0
        {
            return Err(StorageError::Configuration(
                "invalid Mantle cache bounds".to_owned(),
            ));
        }
        Ok(())
    }

    pub fn from_env() -> Result<Self, StorageError> {
        let defaults = Self::default();
        let bytes = |name: &str, default: u64| -> Result<u64, StorageError> {
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
            root: std::env::var_os("LAUNCHER_MANTLE_CACHE_ROOT")
                .map(PathBuf::from)
                .unwrap_or(defaults.root),
            preferred_bytes: bytes(
                "LAUNCHER_MANTLE_CACHE_PREFERRED_BYTES",
                defaults.preferred_bytes,
            )?,
            minimum_bytes: bytes("LAUNCHER_MANTLE_CACHE_MIN_BYTES", defaults.minimum_bytes)?,
            minimum_free_disk_bytes: bytes(
                "LAUNCHER_MANTLE_CACHE_MIN_FREE_DISK_BYTES",
                defaults.minimum_free_disk_bytes,
            )?,
            emergency_free_disk_bytes: bytes(
                "LAUNCHER_MANTLE_CACHE_EMERGENCY_FREE_DISK_BYTES",
                defaults.emergency_free_disk_bytes,
            )?,
            lease_ttl_seconds: bytes(
                "LAUNCHER_MANTLE_CACHE_LEASE_TTL_SECONDS",
                defaults.lease_ttl_seconds,
            )?,
        };
        config.validate()?;
        Ok(config)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MantleCacheEntry {
    pub pack_hash: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub last_accessed: DateTime<Utc>,
    pub pinned: bool,
    pub leased: bool,
    pub verified_elsewhere: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MantleReservation {
    pub reservation_id: String,
    pub bytes: u64,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MantleEviction {
    pub pack_hash: String,
    pub path: PathBuf,
    pub bytes: u64,
}

#[derive(Clone)]
pub struct MantleCache {
    config: Arc<MantleCacheConfig>,
    leases: Arc<Mutex<std::collections::HashMap<String, DateTime<Utc>>>>,
    sequence: Arc<AtomicU64>,
}

impl std::fmt::Debug for MantleCache {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MantleCache")
            .field("config", &self.config)
            .finish()
    }
}

impl MantleCache {
    pub fn new(config: MantleCacheConfig) -> Result<Self, StorageError> {
        config.validate()?;
        Ok(Self {
            config: Arc::new(config),
            leases: Arc::new(Mutex::new(std::collections::HashMap::new())),
            sequence: Arc::new(AtomicU64::new(0)),
        })
    }

    pub fn config(&self) -> &MantleCacheConfig {
        &self.config
    }

    pub async fn reserve(
        &self,
        bytes: u64,
        currently_reserved: u64,
        free_disk_bytes: u64,
    ) -> Result<MantleReservation, StorageError> {
        let safe_free = free_disk_bytes.saturating_sub(self.config.minimum_free_disk_bytes);
        let available = self
            .config
            .preferred_bytes
            .saturating_sub(currently_reserved)
            .min(safe_free);
        if bytes == 0 || bytes > available {
            return Err(StorageError::NeedsCapacity {
                required_bytes: bytes,
                available_bytes: available,
            });
        }
        let id = format!(
            "mantle-{}-{}",
            std::process::id(),
            self.sequence.fetch_add(1, Ordering::AcqRel)
        );
        let expires_at = Utc::now()
            + chrono::Duration::from_std(Duration::from_secs(self.config.lease_ttl_seconds))
                .map_err(|error| StorageError::Configuration(error.to_string()))?;
        self.leases.lock().await.insert(id.clone(), expires_at);
        Ok(MantleReservation {
            reservation_id: id,
            bytes,
            expires_at,
        })
    }

    pub async fn release(&self, reservation_id: &str) {
        self.leases.lock().await.remove(reservation_id);
    }

    pub async fn reconcile_expired_leases(&self) -> usize {
        let now = Utc::now();
        let mut leases = self.leases.lock().await;
        let before = leases.len();
        leases.retain(|_, expires_at| *expires_at > now);
        before - leases.len()
    }

    pub fn eviction_plan(
        &self,
        entries: &[MantleCacheEntry],
        bytes_needed: u64,
    ) -> Vec<MantleEviction> {
        let mut remaining = bytes_needed;
        let mut candidates = entries
            .iter()
            .filter(|entry| !entry.pinned && !entry.leased && entry.verified_elsewhere)
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            left.last_accessed
                .cmp(&right.last_accessed)
                .then_with(|| left.pack_hash.cmp(&right.pack_hash))
        });
        let mut plan = Vec::new();
        for entry in candidates {
            if remaining == 0 {
                break;
            }
            remaining = remaining.saturating_sub(entry.bytes);
            plan.push(MantleEviction {
                pack_hash: entry.pack_hash.clone(),
                path: entry.path.clone(),
                bytes: entry.bytes,
            });
        }
        plan
    }

    pub async fn evict(&self, plan: &[MantleEviction]) -> Result<u64, StorageError> {
        let mut removed: u64 = 0;
        for item in plan {
            match tokio::fs::remove_file(&item.path).await {
                Ok(()) => removed = removed.saturating_add(item.bytes),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(StorageError::Io(error)),
            }
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reservations_are_bounded_and_expirable() {
        let cache = MantleCache::new(MantleCacheConfig {
            root: PathBuf::from("mantle-test"),
            preferred_bytes: 100,
            minimum_bytes: 10,
            minimum_free_disk_bytes: 20,
            emergency_free_disk_bytes: 5,
            lease_ttl_seconds: 1,
        })
        .unwrap();
        assert!(cache.reserve(81, 0, 100).await.is_err());
        let reservation = cache.reserve(50, 0, 100).await.unwrap();
        cache.release(&reservation.reservation_id).await;
        assert_eq!(cache.reconcile_expired_leases().await, 0);
    }

    #[test]
    fn eviction_never_selects_pinned_or_unverified_entries() {
        let cache = MantleCache::new(MantleCacheConfig::default()).unwrap();
        let now = Utc::now();
        let entries = vec![
            MantleCacheEntry {
                pack_hash: "a".repeat(64),
                path: PathBuf::from("a"),
                bytes: 10,
                last_accessed: now - chrono::Duration::hours(2),
                pinned: false,
                leased: false,
                verified_elsewhere: false,
            },
            MantleCacheEntry {
                pack_hash: "b".repeat(64),
                path: PathBuf::from("b"),
                bytes: 10,
                last_accessed: now - chrono::Duration::hours(1),
                pinned: true,
                leased: false,
                verified_elsewhere: true,
            },
            MantleCacheEntry {
                pack_hash: "c".repeat(64),
                path: PathBuf::from("c"),
                bytes: 10,
                last_accessed: now,
                pinned: false,
                leased: false,
                verified_elsewhere: true,
            },
        ];
        let plan = cache.eviction_plan(&entries, 10);
        assert_eq!(plan[0].pack_hash, "c".repeat(64));
    }
}
