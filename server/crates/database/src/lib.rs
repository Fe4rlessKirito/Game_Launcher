use async_trait::async_trait;
use chrono::{DateTime, Utc};
use launcher_common::{BuildSummary, CatalogPage, GameSummary, Manifest, ManifestSignature};
use launcher_storage::{
    CapacityReservationStore, CapacitySnapshot, MegaAccountConfig, StorageAccountSnapshot,
    StorageAccountStatus, StorageClass, StorageError, StoragePolicy, StoragePool,
    StorageReservation, StorageTier,
};
use sqlx::{
    PgPool, Row,
    postgres::{PgPoolOptions, PgRow},
};
use std::collections::HashMap;
use std::time::Duration;
use thiserror::Error;
use uuid::Uuid;

mod provisioning;

#[derive(Debug, Error)]
pub enum DatabaseError {
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("invalid database integer")]
    Integer,
    #[error("manifest is missing or invalid: {0}")]
    Manifest(String),
}

#[derive(Debug, Clone)]
pub struct ClaimedIngestionJob {
    pub id: i64,
    pub build_id: Option<String>,
    pub stage: String,
    pub attempts: i32,
    pub lease_until: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageLocationRecord {
    pub provider: String,
    pub pool_id: String,
    pub failure_domain: String,
    pub tier: StorageTier,
    pub object_key: String,
    pub direct_url: String,
    pub priority: i32,
    pub verified_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct StorageAccountRecord {
    pub snapshot: StorageAccountSnapshot,
    pub credential_reference: String,
    pub pool_id: String,
    pub failure_domain: String,
    pub configuration_json: serde_json::Value,
    pub last_health_check: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct RestoreJob {
    pub id: i64,
    pub encoded_hash: String,
    pub target_provider: String,
    pub state: String,
    pub attempts: i32,
    pub max_attempts: i32,
    pub next_attempt_at: DateTime<Utc>,
    pub lease_until: Option<DateTime<Utc>>,
    pub worker_id: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct StorageObjectRecord {
    pub encoded_hash: String,
    pub encoded_size: i64,
    pub provider: String,
    pub pool_id: String,
    pub failure_domain: String,
    pub tier: StorageTier,
    pub object_key: String,
    pub verified_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaTableStatus {
    pub table: String,
    pub present: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaStatus {
    pub tables: Vec<SchemaTableStatus>,
}

impl SchemaStatus {
    pub fn ready(&self) -> bool {
        self.tables.iter().all(|table| table.present)
    }
}

fn parse_storage_tier(value: &str) -> Result<StorageTier, DatabaseError> {
    value
        .parse()
        .map_err(|error: StorageError| DatabaseError::Manifest(error.to_string()))
}

fn storage_error(error: impl std::fmt::Display) -> StorageError {
    StorageError::Unavailable(format!("database storage operation failed: {error}"))
}

fn u64_to_i64(value: u64) -> Result<i64, DatabaseError> {
    i64::try_from(value).map_err(|_| DatabaseError::Integer)
}

fn i64_to_u64(value: i64) -> Result<u64, DatabaseError> {
    u64::try_from(value).map_err(|_| DatabaseError::Integer)
}

fn parse_storage_status(value: &str) -> Result<StorageAccountStatus, DatabaseError> {
    match value {
        "ACTIVE" => Ok(StorageAccountStatus::Active),
        "NEAR_FULL" => Ok(StorageAccountStatus::NearFull),
        "FULL" => Ok(StorageAccountStatus::Full),
        "UNAVAILABLE" => Ok(StorageAccountStatus::Unavailable),
        "AUTH_FAILED" => Ok(StorageAccountStatus::AuthFailed),
        "DISABLED" => Ok(StorageAccountStatus::Disabled),
        "NEEDS_REAUTH" => Ok(StorageAccountStatus::NeedsReauth),
        other => Err(DatabaseError::Manifest(format!(
            "unknown storage account status {other:?}"
        ))),
    }
}

fn account_snapshot_from_row(row: &PgRow) -> Result<StorageAccountSnapshot, DatabaseError> {
    let provider_id: String = row.try_get("provider_id")?;
    let pool_id: String = row
        .try_get("pool_id")
        .unwrap_or_else(|_| provider_id.clone());
    let failure_domain: String = row
        .try_get("failure_domain")
        .unwrap_or_else(|_| pool_id.clone());
    Ok(StorageAccountSnapshot {
        account_id: row.try_get("id")?,
        provider_id,
        pool_id,
        failure_domain,
        tier: parse_storage_tier(row.try_get::<String, _>("tier")?.as_str())?,
        status: parse_storage_status(row.try_get::<String, _>("status")?.as_str())?,
        capacity_bytes: i64_to_u64(row.try_get("capacity_bytes")?)?,
        used_bytes: i64_to_u64(row.try_get("used_bytes")?)?,
        reserved_bytes: i64_to_u64(row.try_get("reserved_bytes")?)?,
        safety_margin_bytes: i64_to_u64(row.try_get("safety_margin_bytes")?)?,
        last_capacity_check: row.try_get("last_capacity_check")?,
    })
}

fn restore_job_from_row(row: &PgRow) -> Result<RestoreJob, DatabaseError> {
    Ok(RestoreJob {
        id: row.try_get("id")?,
        encoded_hash: row.try_get("encoded_hash")?,
        target_provider: row.try_get("target_provider")?,
        state: row.try_get("state")?,
        attempts: row.try_get("attempts")?,
        max_attempts: row.try_get("max_attempts")?,
        next_attempt_at: row.try_get("next_attempt_at")?,
        lease_until: row.try_get("lease_until")?,
        worker_id: row.try_get("worker_id")?,
        last_error: row.try_get("last_error")?,
    })
}

#[derive(Clone)]
pub struct Database {
    pool: PgPool,
}

impl Database {
    pub async fn connect(url: &str) -> Result<Self, DatabaseError> {
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .connect(url)
            .await?;
        Ok(Self { pool })
    }

    pub async fn migrate(&self) -> Result<(), DatabaseError> {
        let initial = include_str!("../../../../migrations/001_initial.sql");
        sqlx::raw_sql(initial).execute(&self.pool).await?;
        let storage_tiering = include_str!("../../../../migrations/002_storage_tiering.sql");
        sqlx::raw_sql(storage_tiering).execute(&self.pool).await?;
        let storage_pools = include_str!("../../../../migrations/003_storage_pools.sql");
        sqlx::raw_sql(storage_pools).execute(&self.pool).await?;
        let provisioning = include_str!("../../../../migrations/004_provisioning.sql");
        sqlx::raw_sql(provisioning).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn ping(&self) -> Result<(), DatabaseError> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }

    pub async fn schema_status(&self) -> Result<SchemaStatus, DatabaseError> {
        let rows = sqlx::query(
            "WITH required_tables AS (
                 SELECT table_name
                 FROM unnest(ARRAY[
                     'games', 'builds', 'chunks', 'build_chunks', 'storage_locations',
                     'storage_objects', 'ingestion_jobs', 'storage_providers', 'storage_pools', 'storage_accounts',
                     'storage_reservations', 'storage_health_events', 'restore_jobs',
                     'provisioning_jobs', 'provisioning_job_events',
                     'provisioning_mail_messages', 'provisioning_mail_nonces'
                 ]::text[]) AS table_name
             ),
             checks AS (
                 SELECT 'table:' || table_name AS check_name,
                        to_regclass('public.' || table_name) IS NOT NULL AS present
                 FROM required_tables
                 UNION ALL
                 SELECT 'column:storage_locations.tier',
                        EXISTS(
                            SELECT 1 FROM information_schema.columns
                            WHERE table_schema='public'
                              AND table_name='storage_locations'
                              AND column_name='tier'
                        )
                 UNION ALL
                 SELECT 'column:storage_objects.tier',
                        EXISTS(
                            SELECT 1 FROM information_schema.columns
                            WHERE table_schema='public'
                              AND table_name='storage_objects'
                              AND column_name='tier'
                        )
                UNION ALL
                SELECT 'column:storage_providers.pool_id',
                       EXISTS(
                           SELECT 1 FROM information_schema.columns
                           WHERE table_schema='public'
                             AND table_name='storage_providers'
                             AND column_name='pool_id'
                       )
                UNION ALL
                SELECT 'column:storage_locations.pool_id',
                       EXISTS(
                           SELECT 1 FROM information_schema.columns
                           WHERE table_schema='public'
                             AND table_name='storage_locations'
                             AND column_name='pool_id'
                       )
                UNION ALL
                SELECT 'primary_key:storage_objects(encoded_hash,provider)',
                        EXISTS(
                            SELECT 1
                            FROM pg_constraint
                            WHERE conrelid=to_regclass('public.storage_objects')
                              AND contype='p'
                              AND pg_get_constraintdef(oid) ILIKE '%provider%'
                        )
             )
             SELECT check_name AS table_name, present
             FROM checks
             ORDER BY table_name",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(SchemaStatus {
            tables: rows
                .iter()
                .map(|row| {
                    Ok(SchemaTableStatus {
                        table: row.try_get("table_name")?,
                        present: row.try_get("present")?,
                    })
                })
                .collect::<Result<Vec<_>, sqlx::Error>>()?,
        })
    }

    pub async fn list_published_games(
        &self,
        limit: u32,
        offset: u32,
    ) -> Result<CatalogPage, DatabaseError> {
        let rows = sqlx::query(
            "SELECT g.id, g.slug, g.title, g.description, g.hero_image_url, g.cover_image_url,
                    b.id AS build_id, b.display_version, b.size_bytes, b.published_at
             FROM games g
             LEFT JOIN LATERAL (
                 SELECT id, display_version, size_bytes, published_at
                 FROM builds WHERE game_id = g.id AND state = 'PUBLISHED'
                 ORDER BY published_at DESC NULLS LAST LIMIT 1
             ) b ON TRUE
             ORDER BY g.title, g.id LIMIT $1 OFFSET $2",
        )
        .bind(i64::from(limit.min(100)))
        .bind(i64::from(offset))
        .fetch_all(&self.pool)
        .await?;
        let items = rows
            .into_iter()
            .map(|row| {
                let build_id: Option<String> = row.try_get("build_id")?;
                let latest_build = build_id.map(|id| BuildSummary {
                    game_id: row.try_get("id").unwrap_or_default(),
                    display_version: row.try_get("display_version").unwrap_or_default(),
                    size_bytes: row
                        .try_get::<i64, _>("size_bytes")
                        .unwrap_or_default()
                        .max(0) as u64,
                    published_at: row
                        .try_get::<Option<DateTime<Utc>>, _>("published_at")
                        .unwrap_or(None),
                    id,
                });
                Ok(GameSummary {
                    id: row.try_get("id")?,
                    slug: row.try_get("slug")?,
                    title: row.try_get("title")?,
                    description: row.try_get("description")?,
                    hero_image_url: row.try_get("hero_image_url")?,
                    cover_image_url: row.try_get("cover_image_url")?,
                    latest_build,
                })
            })
            .collect::<Result<Vec<_>, sqlx::Error>>()?;
        let next_cursor = (items.len() as u32 == limit.min(100))
            .then(|| (offset + items.len() as u32).to_string());
        Ok(CatalogPage { items, next_cursor })
    }

    pub async fn get_game(&self, id: &str) -> Result<Option<GameSummary>, DatabaseError> {
        Ok(self
            .list_published_games(100, 0)
            .await?
            .items
            .into_iter()
            .find(|game| game.id == id || game.slug == id))
    }

    pub async fn upsert_game(&self, game: &GameSummary) -> Result<(), DatabaseError> {
        sqlx::query("INSERT INTO games(id, slug, title, description, hero_image_url, cover_image_url) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(id) DO UPDATE SET slug=excluded.slug, title=excluded.title, description=excluded.description, hero_image_url=excluded.hero_image_url, cover_image_url=excluded.cover_image_url")
            .bind(&game.id).bind(&game.slug).bind(&game.title).bind(&game.description).bind(&game.hero_image_url).bind(&game.cover_image_url).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn upsert_build(
        &self,
        manifest: &Manifest,
        signature: Option<&ManifestSignature>,
        state: &str,
    ) -> Result<(), DatabaseError> {
        let manifest_bytes = serde_json::to_vec(manifest)
            .map_err(|error| DatabaseError::Manifest(error.to_string()))?;
        self.upsert_build_with_bytes(manifest, &manifest_bytes, signature, state)
            .await
    }

    pub async fn upsert_build_with_bytes(
        &self,
        manifest: &Manifest,
        manifest_bytes: &[u8],
        signature: Option<&ManifestSignature>,
        state: &str,
    ) -> Result<(), DatabaseError> {
        manifest
            .validate()
            .map_err(|error| DatabaseError::Manifest(error.to_string()))?;
        let manifest_json = serde_json::to_value(manifest)
            .map_err(|error| DatabaseError::Manifest(error.to_string()))?;
        let signature_json = signature
            .map(serde_json::to_value)
            .transpose()
            .map_err(|error| DatabaseError::Manifest(error.to_string()))?;
        let size_bytes = manifest
            .files
            .iter()
            .map(|file| file.size as i64)
            .sum::<i64>();
        sqlx::query("INSERT INTO builds(id, game_id, display_version, state, size_bytes, manifest_json, manifest_bytes, signature_json) VALUES($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT(id) DO UPDATE SET display_version=excluded.display_version, state=excluded.state, size_bytes=excluded.size_bytes, manifest_json=excluded.manifest_json, manifest_bytes=excluded.manifest_bytes, signature_json=excluded.signature_json")
            .bind(&manifest.build_id).bind(&manifest.game_id).bind(&manifest.display_version).bind(state).bind(size_bytes).bind(manifest_json).bind(manifest_bytes).bind(signature_json).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn add_chunk(
        &self,
        encoded_hash: &str,
        encoded_size: i64,
        encoding_id: &str,
    ) -> Result<(), DatabaseError> {
        sqlx::query("INSERT INTO chunks(encoded_hash, encoded_size, encoding_id) VALUES($1,$2,$3) ON CONFLICT(encoded_hash) DO UPDATE SET encoded_size=excluded.encoded_size, encoding_id=excluded.encoding_id")
            .bind(encoded_hash).bind(encoded_size).bind(encoding_id).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn attach_build_chunk(
        &self,
        build_id: &str,
        encoded_hash: &str,
        raw_size: i64,
        raw_hash: &str,
        ordinal: i32,
    ) -> Result<(), DatabaseError> {
        sqlx::query("INSERT INTO build_chunks(build_id, encoded_hash, raw_size, raw_hash, ordinal) VALUES($1,$2,$3,$4,$5) ON CONFLICT(build_id, encoded_hash, ordinal) DO UPDATE SET raw_size=excluded.raw_size, raw_hash=excluded.raw_hash")
            .bind(build_id).bind(encoded_hash).bind(raw_size).bind(raw_hash).bind(ordinal).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn add_storage_location(
        &self,
        encoded_hash: &str,
        provider: &str,
        object_key: &str,
        direct_url: &str,
        priority: i32,
    ) -> Result<(), DatabaseError> {
        self.add_storage_location_with_tier(
            encoded_hash,
            provider,
            StorageTier::Hot,
            object_key,
            direct_url,
            priority,
        )
        .await
    }

    pub async fn add_storage_location_with_tier(
        &self,
        encoded_hash: &str,
        provider: &str,
        tier: StorageTier,
        object_key: &str,
        direct_url: &str,
        priority: i32,
    ) -> Result<(), DatabaseError> {
        self.add_storage_location_with_pool(
            encoded_hash,
            provider,
            provider,
            provider,
            tier,
            object_key,
            direct_url,
            priority,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn add_storage_location_with_pool(
        &self,
        encoded_hash: &str,
        provider: &str,
        pool_id: &str,
        failure_domain: &str,
        tier: StorageTier,
        object_key: &str,
        direct_url: &str,
        priority: i32,
    ) -> Result<(), DatabaseError> {
        self.ensure_storage_pool_compatibility(pool_id, tier, provider, failure_domain)
            .await?;
        sqlx::query("INSERT INTO storage_locations(encoded_hash, provider, pool_id, failure_domain, tier, object_key, direct_url, priority, verified_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,now()) ON CONFLICT(encoded_hash, provider, direct_url) DO UPDATE SET pool_id=excluded.pool_id, failure_domain=excluded.failure_domain, tier=excluded.tier, object_key=excluded.object_key, priority=excluded.priority, verified_at=now()")
            .bind(encoded_hash)
            .bind(provider)
            .bind(pool_id)
            .bind(failure_domain)
            .bind(tier.as_str())
            .bind(object_key)
            .bind(direct_url)
            .bind(priority)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn add_storage_object(
        &self,
        encoded_hash: &str,
        encoded_size: i64,
        provider: &str,
        object_key: &str,
    ) -> Result<(), DatabaseError> {
        self.add_storage_object_with_tier(
            encoded_hash,
            encoded_size,
            provider,
            StorageTier::Hot,
            object_key,
        )
        .await
    }

    pub async fn add_storage_object_with_tier(
        &self,
        encoded_hash: &str,
        encoded_size: i64,
        provider: &str,
        tier: StorageTier,
        object_key: &str,
    ) -> Result<(), DatabaseError> {
        self.add_storage_object_with_pool(
            encoded_hash,
            encoded_size,
            provider,
            provider,
            provider,
            tier,
            object_key,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn add_storage_object_with_pool(
        &self,
        encoded_hash: &str,
        encoded_size: i64,
        provider: &str,
        pool_id: &str,
        failure_domain: &str,
        tier: StorageTier,
        object_key: &str,
    ) -> Result<(), DatabaseError> {
        self.ensure_storage_pool_compatibility(pool_id, tier, provider, failure_domain)
            .await?;
        sqlx::query("INSERT INTO storage_objects(encoded_hash, encoded_size, provider, pool_id, failure_domain, tier, object_key, verified_at) VALUES($1,$2,$3,$4,$5,$6,$7,now()) ON CONFLICT(encoded_hash, provider) DO UPDATE SET encoded_size=excluded.encoded_size, pool_id=excluded.pool_id, failure_domain=excluded.failure_domain, tier=excluded.tier, object_key=excluded.object_key, verified_at=now()")
            .bind(encoded_hash)
            .bind(encoded_size)
            .bind(provider)
            .bind(pool_id)
            .bind(failure_domain)
            .bind(tier.as_str())
            .bind(object_key)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn ensure_storage_pool_compatibility(
        &self,
        pool_id: &str,
        class: StorageClass,
        provider_type: &str,
        failure_domain: &str,
    ) -> Result<(), DatabaseError> {
        sqlx::query(
            "INSERT INTO storage_pools(
                id, storage_class, provider_type, priority, failure_domain,
                enabled, status, provisioning_mode
             ) VALUES($1,$2,$3,100,$4,TRUE,'READY',
                      CASE WHEN lower($3) LIKE '%mega%' THEN 'MANUAL' ELSE 'DISABLED' END)
             ON CONFLICT(id) DO NOTHING",
        )
        .bind(pool_id)
        .bind(class.as_str())
        .bind(provider_type)
        .bind(failure_domain)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn ensure_storage_pools(&self, pools: &[StoragePool]) -> Result<(), DatabaseError> {
        for pool in pools {
            self.ensure_storage_pool_compatibility(
                &pool.id,
                pool.storage_class,
                &pool.provider_type,
                &pool.failure_domain,
            )
            .await?;
        }
        Ok(())
    }

    pub async fn get_storage_locations(
        &self,
        encoded_hashes: &[String],
    ) -> Result<HashMap<String, Vec<StorageLocationRecord>>, DatabaseError> {
        if encoded_hashes.is_empty() {
            return Ok(HashMap::new());
        }
        let rows = sqlx::query(
            "SELECT encoded_hash, provider,
                    COALESCE(pool_id, provider) AS pool_id,
                    COALESCE(failure_domain, pool_id, provider) AS failure_domain,
                    tier, object_key, direct_url, priority, verified_at
             FROM storage_locations
             WHERE encoded_hash = ANY($1) AND verified_at IS NOT NULL
             ORDER BY encoded_hash, priority, provider, direct_url",
        )
        .bind(encoded_hashes)
        .fetch_all(&self.pool)
        .await?;
        let mut locations = HashMap::<String, Vec<StorageLocationRecord>>::new();
        for row in rows {
            let hash: String = row.try_get("encoded_hash")?;
            locations
                .entry(hash)
                .or_default()
                .push(StorageLocationRecord {
                    provider: row.try_get("provider")?,
                    pool_id: row.try_get("pool_id")?,
                    failure_domain: row.try_get("failure_domain")?,
                    tier: parse_storage_tier(row.try_get::<String, _>("tier")?.as_str())?,
                    object_key: row.try_get("object_key")?,
                    direct_url: row.try_get("direct_url")?,
                    priority: row.try_get("priority")?,
                    verified_at: row.try_get("verified_at")?,
                });
        }
        Ok(locations)
    }

    pub async fn get_storage_locations_for_tier(
        &self,
        encoded_hashes: &[String],
        tier: StorageTier,
    ) -> Result<HashMap<String, Vec<StorageLocationRecord>>, DatabaseError> {
        let locations = self.get_storage_locations(encoded_hashes).await?;
        Ok(locations
            .into_iter()
            .map(|(hash, records)| {
                (
                    hash,
                    records
                        .into_iter()
                        .filter(|record| record.tier == tier)
                        .collect(),
                )
            })
            .filter(|(_, records): &(String, Vec<StorageLocationRecord>)| !records.is_empty())
            .collect())
    }

    pub async fn publish_build(&self, build_id: &str) -> Result<(), DatabaseError> {
        self.publish_build_with_policy(build_id, 1, 0).await
    }

    pub async fn publish_build_with_policy(
        &self,
        build_id: &str,
        required_hot_replicas: u32,
        required_cold_replicas: u32,
    ) -> Result<(), DatabaseError> {
        let mut transaction = self.pool.begin().await?;
        let row = sqlx::query(
            "WITH replicas AS (
                SELECT encoded_hash, provider, tier
                FROM storage_objects
                WHERE verified_at IS NOT NULL
                UNION
                SELECT encoded_hash, provider, tier
                FROM storage_locations
                WHERE verified_at IS NOT NULL
            )
            SELECT COUNT(*) AS missing
            FROM build_chunks bc
            WHERE bc.build_id = $1
              AND ((
                  SELECT COUNT(DISTINCT provider)
                  FROM replicas
                  WHERE encoded_hash = bc.encoded_hash AND tier = 'HOT'
              ) < $2
              OR (
                  SELECT COUNT(DISTINCT provider)
                  FROM replicas
                  WHERE encoded_hash = bc.encoded_hash AND tier = 'COLD'
              ) < $3)",
        )
        .bind(build_id)
        .bind(i64::from(required_hot_replicas))
        .bind(i64::from(required_cold_replicas))
        .fetch_one(&mut *transaction)
        .await?;
        let missing: i64 = row.try_get("missing")?;
        if missing != 0 {
            return Err(DatabaseError::Manifest(format!(
                "cannot publish build {build_id}: {missing} chunks do not satisfy the hot/cold storage policy"
            )));
        }
        let result = sqlx::query("UPDATE builds SET state='PUBLISHED', published_at=now() WHERE id=$1 AND state IN ('READY','VERIFIED')").bind(build_id).execute(&mut *transaction).await?;
        if result.rows_affected() != 1 {
            return Err(DatabaseError::Manifest(format!(
                "build {build_id} is not publishable"
            )));
        }
        transaction.commit().await?;
        Ok(())
    }

    pub async fn publish_build_with_storage_policy(
        &self,
        build_id: &str,
        policy: &StoragePolicy,
    ) -> Result<(), DatabaseError> {
        let mut transaction = self.pool.begin().await?;
        let row = sqlx::query(
            "WITH replicas AS (
                SELECT encoded_hash, tier, provider,
                       COALESCE(pool_id, provider) AS pool_id,
                       COALESCE(failure_domain, pool_id, provider) AS failure_domain
                FROM storage_objects
                WHERE verified_at IS NOT NULL
                UNION
                SELECT encoded_hash, tier, provider,
                       COALESCE(pool_id, provider) AS pool_id,
                       COALESCE(failure_domain, pool_id, provider) AS failure_domain
                FROM storage_locations
                WHERE verified_at IS NOT NULL
            )
            SELECT COUNT(*) AS missing
            FROM build_chunks bc
            WHERE bc.build_id = $1
              AND (
                (SELECT COUNT(DISTINCT provider) FROM replicas
                 WHERE encoded_hash=bc.encoded_hash AND tier='HOT') < $2
                OR (SELECT COUNT(DISTINCT failure_domain) FROM replicas
                 WHERE encoded_hash=bc.encoded_hash AND tier='HOT') < $3
                OR (SELECT COUNT(DISTINCT provider) FROM replicas
                 WHERE encoded_hash=bc.encoded_hash AND tier='COLD') < $4
                OR (SELECT COUNT(DISTINCT failure_domain) FROM replicas
                 WHERE encoded_hash=bc.encoded_hash AND tier='COLD') < $5
                OR (SELECT COUNT(DISTINCT provider) FROM replicas
                 WHERE encoded_hash=bc.encoded_hash AND tier='ARCHIVE') < $6
                OR (SELECT COUNT(DISTINCT failure_domain) FROM replicas
                 WHERE encoded_hash=bc.encoded_hash AND tier='ARCHIVE') < $7
              )",
        )
        .bind(build_id)
        .bind(i64::from(policy.required_replicas(StorageClass::Hot)))
        .bind(i64::from(
            policy.required_failure_domains(StorageClass::Hot),
        ))
        .bind(i64::from(policy.required_replicas(StorageClass::Cold)))
        .bind(i64::from(
            policy.required_failure_domains(StorageClass::Cold),
        ))
        .bind(i64::from(policy.required_replicas(StorageClass::Archive)))
        .bind(i64::from(
            policy.required_failure_domains(StorageClass::Archive),
        ))
        .fetch_one(&mut *transaction)
        .await?;
        let missing: i64 = row.try_get("missing")?;
        if missing != 0 {
            return Err(DatabaseError::Manifest(format!(
                "cannot publish build {build_id}: {missing} chunks do not satisfy the storage class/pool policy"
            )));
        }
        let result = sqlx::query("UPDATE builds SET state='PUBLISHED', published_at=now() WHERE id=$1 AND state IN ('READY','VERIFIED')")
            .bind(build_id)
            .execute(&mut *transaction)
            .await?;
        if result.rows_affected() != 1 {
            return Err(DatabaseError::Manifest(format!(
                "build {build_id} is not publishable"
            )));
        }
        transaction.commit().await?;
        Ok(())
    }

    pub async fn upsert_storage_provider(
        &self,
        provider_id: &str,
        kind: &str,
        tier: StorageTier,
        configuration_json: serde_json::Value,
    ) -> Result<(), DatabaseError> {
        sqlx::query(
            "INSERT INTO storage_pools(
                id, storage_class, provider_type, priority, failure_domain,
                enabled, status, provisioning_mode, configuration_json, updated_at
             ) VALUES($1,$2,$3,100,CASE WHEN lower($3)='mega' THEN 'mega' ELSE $1 END,TRUE,'READY',
                      CASE WHEN lower($3)='mega' THEN 'MANUAL' ELSE 'DISABLED' END,$4,now())
             ON CONFLICT(id) DO UPDATE SET storage_class=excluded.storage_class,
                 provider_type=excluded.provider_type,
                 configuration_json=excluded.configuration_json, updated_at=now()",
        )
        .bind(provider_id)
        .bind(tier.as_str())
        .bind(kind)
        .bind(&configuration_json)
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "INSERT INTO storage_providers(id, kind, tier, pool_id, configuration_json, updated_at)
             VALUES($1,$2,$3,$1,$4,now())
             ON CONFLICT(id) DO UPDATE SET kind=excluded.kind, tier=excluded.tier,
                 pool_id=excluded.pool_id,
                 configuration_json=excluded.configuration_json, updated_at=now()",
        )
        .bind(provider_id)
        .bind(kind)
        .bind(tier.as_str())
        .bind(configuration_json)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn upsert_storage_account(
        &self,
        provider_id: &str,
        config: &MegaAccountConfig,
        status: StorageAccountStatus,
    ) -> Result<(), DatabaseError> {
        let configuration_json = serde_json::to_value(config)
            .map_err(|error| DatabaseError::Manifest(error.to_string()))?;
        let capacity_bytes = u64_to_i64(config.capacity_bytes)?;
        let safety_margin_bytes = u64_to_i64(config.safety_margin_bytes)?;
        sqlx::query(
            "INSERT INTO storage_accounts(
                id, provider_id, pool_id, failure_domain, credential_reference, tier, status,
                capacity_bytes, safety_margin_bytes, configuration_json, updated_at
             ) VALUES($1,$2,$2,$2,$3,'COLD',$4,$5,$6,$7,now())
             ON CONFLICT(id) DO UPDATE SET provider_id=excluded.provider_id,
                 pool_id=excluded.pool_id, failure_domain=excluded.failure_domain,
                 credential_reference=excluded.credential_reference,
                 tier=excluded.tier, status=excluded.status,
                 capacity_bytes=excluded.capacity_bytes,
                 safety_margin_bytes=excluded.safety_margin_bytes,
                 configuration_json=excluded.configuration_json, updated_at=now()",
        )
        .bind(&config.account_id)
        .bind(provider_id)
        .bind(&config.credential_reference)
        .bind(status.as_str())
        .bind(capacity_bytes)
        .bind(safety_margin_bytes)
        .bind(configuration_json)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_storage_accounts(
        &self,
        provider_id: Option<&str>,
    ) -> Result<Vec<StorageAccountRecord>, DatabaseError> {
        let rows = if let Some(provider_id) = provider_id {
            sqlx::query(
                "SELECT id, provider_id, COALESCE(pool_id, provider_id) AS pool_id,
                        COALESCE(failure_domain, pool_id, provider_id) AS failure_domain,
                        credential_reference, tier, status,
                        capacity_bytes, used_bytes, reserved_bytes, safety_margin_bytes,
                        configuration_json, last_capacity_check, last_health_check
                 FROM storage_accounts WHERE provider_id=$1 ORDER BY id",
            )
            .bind(provider_id)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query(
                "SELECT id, provider_id, COALESCE(pool_id, provider_id) AS pool_id,
                        COALESCE(failure_domain, pool_id, provider_id) AS failure_domain,
                        credential_reference, tier, status,
                        capacity_bytes, used_bytes, reserved_bytes, safety_margin_bytes,
                        configuration_json, last_capacity_check, last_health_check
                 FROM storage_accounts ORDER BY provider_id, id",
            )
            .fetch_all(&self.pool)
            .await?
        };
        rows.iter()
            .map(|row| {
                Ok(StorageAccountRecord {
                    snapshot: account_snapshot_from_row(row)?,
                    credential_reference: row.try_get("credential_reference")?,
                    pool_id: row.try_get("pool_id")?,
                    failure_domain: row.try_get("failure_domain")?,
                    configuration_json: row.try_get("configuration_json")?,
                    last_health_check: row.try_get("last_health_check")?,
                })
            })
            .collect()
    }

    pub async fn set_storage_account_status(
        &self,
        account_id: &str,
        status: StorageAccountStatus,
        error: Option<&str>,
    ) -> Result<(), DatabaseError> {
        let mut transaction = self.pool.begin().await?;
        let row = sqlx::query("SELECT provider_id FROM storage_accounts WHERE id=$1 FOR UPDATE")
            .bind(account_id)
            .fetch_optional(&mut *transaction)
            .await?;
        let Some(row) = row else {
            return Err(DatabaseError::Manifest(format!(
                "unknown storage account {account_id}"
            )));
        };
        let provider_id: String = row.try_get("provider_id")?;
        sqlx::query(
            "UPDATE storage_accounts SET status=$1, last_health_check=now(), updated_at=now()
             WHERE id=$2",
        )
        .bind(status.as_str())
        .bind(account_id)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "INSERT INTO storage_health_events(provider_id, account_id, status, error)
             VALUES($1,$2,$3,$4)",
        )
        .bind(provider_id)
        .bind(account_id)
        .bind(status.as_str())
        .bind(error)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn list_storage_objects(
        &self,
        encoded_hashes: &[String],
    ) -> Result<Vec<StorageObjectRecord>, DatabaseError> {
        if encoded_hashes.is_empty() {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(
            "SELECT encoded_hash, encoded_size, provider,
                    COALESCE(pool_id, provider) AS pool_id,
                    COALESCE(failure_domain, pool_id, provider) AS failure_domain,
                    tier, object_key, verified_at
             FROM storage_objects
             WHERE encoded_hash = ANY($1)
             ORDER BY encoded_hash, tier, provider",
        )
        .bind(encoded_hashes)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(StorageObjectRecord {
                    encoded_hash: row.try_get("encoded_hash")?,
                    encoded_size: row.try_get("encoded_size")?,
                    provider: row.try_get("provider")?,
                    pool_id: row.try_get("pool_id")?,
                    failure_domain: row.try_get("failure_domain")?,
                    tier: parse_storage_tier(row.try_get::<String, _>("tier")?.as_str())?,
                    object_key: row.try_get("object_key")?,
                    verified_at: row.try_get("verified_at")?,
                })
            })
            .collect()
    }

    pub async fn count_published_build_references(
        &self,
        encoded_hash: &str,
    ) -> Result<i64, DatabaseError> {
        let row = sqlx::query(
            "SELECT COUNT(DISTINCT b.id) AS references
             FROM build_chunks bc
             JOIN builds b ON b.id = bc.build_id
             WHERE bc.encoded_hash = $1
               AND b.state IN ('PUBLISHED','READY','VERIFIED')",
        )
        .bind(encoded_hash)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.try_get("references")?)
    }

    pub async fn list_unreachable_storage_objects(
        &self,
        limit: u32,
    ) -> Result<Vec<StorageObjectRecord>, DatabaseError> {
        let rows = sqlx::query(
            "SELECT so.encoded_hash, so.encoded_size, so.provider,
                    COALESCE(so.pool_id, so.provider) AS pool_id,
                    COALESCE(so.failure_domain, so.pool_id, so.provider) AS failure_domain,
                    so.tier, so.object_key, so.verified_at
             FROM storage_objects so
             WHERE NOT EXISTS (
                 SELECT 1
                 FROM build_chunks bc
                 JOIN builds b ON b.id = bc.build_id
                 WHERE bc.encoded_hash = so.encoded_hash
                   AND b.state IN ('PUBLISHED','READY','VERIFIED')
             )
             ORDER BY so.created_at, so.encoded_hash, so.provider
             LIMIT $1",
        )
        .bind(i64::from(limit.clamp(1, 5000)))
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(StorageObjectRecord {
                    encoded_hash: row.try_get("encoded_hash")?,
                    encoded_size: row.try_get("encoded_size")?,
                    provider: row.try_get("provider")?,
                    pool_id: row.try_get("pool_id")?,
                    failure_domain: row.try_get("failure_domain")?,
                    tier: parse_storage_tier(row.try_get::<String, _>("tier")?.as_str())?,
                    object_key: row.try_get("object_key")?,
                    verified_at: row.try_get("verified_at")?,
                })
            })
            .collect()
    }

    pub async fn delete_storage_object(
        &self,
        encoded_hash: &str,
        provider: &str,
    ) -> Result<(), DatabaseError> {
        sqlx::query("DELETE FROM storage_objects WHERE encoded_hash=$1 AND provider=$2")
            .bind(encoded_hash)
            .bind(provider)
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM storage_locations WHERE encoded_hash=$1 AND provider=$2")
            .bind(encoded_hash)
            .bind(provider)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn enqueue_restore_job(
        &self,
        encoded_hash: &str,
        target_provider: &str,
    ) -> Result<RestoreJob, DatabaseError> {
        let mut transaction = self.pool.begin().await?;
        if let Some(row) = sqlx::query(
            "SELECT id, encoded_hash, target_provider, state, attempts, max_attempts,
                    next_attempt_at, lease_until, worker_id, last_error
             FROM restore_jobs
             WHERE encoded_hash=$1 AND target_provider=$2
               AND state IN ('QUEUED','RUNNING','RETRY')
             ORDER BY id LIMIT 1",
        )
        .bind(encoded_hash)
        .bind(target_provider)
        .fetch_optional(&mut *transaction)
        .await?
        {
            let job = restore_job_from_row(&row)?;
            transaction.commit().await?;
            return Ok(job);
        }
        let row = sqlx::query(
            "INSERT INTO restore_jobs(encoded_hash, target_provider)
             VALUES($1,$2)
             RETURNING id, encoded_hash, target_provider, state, attempts, max_attempts,
                       next_attempt_at, lease_until, worker_id, last_error",
        )
        .bind(encoded_hash)
        .bind(target_provider)
        .fetch_one(&mut *transaction)
        .await?;
        let job = restore_job_from_row(&row)?;
        transaction.commit().await?;
        Ok(job)
    }

    pub async fn claim_restore_job(
        &self,
        worker_id: &str,
        lease_seconds: i64,
    ) -> Result<Option<RestoreJob>, DatabaseError> {
        let mut transaction = self.pool.begin().await?;
        let row = sqlx::query(
            "SELECT id, encoded_hash, target_provider, state, attempts, max_attempts,
                    next_attempt_at, lease_until, worker_id, last_error
             FROM restore_jobs
             WHERE attempts < max_attempts
               AND next_attempt_at <= now()
               AND (
                   state IN ('QUEUED','RETRY')
                   OR (state='RUNNING' AND (lease_until IS NULL OR lease_until < now()))
               )
             ORDER BY updated_at, id
             FOR UPDATE SKIP LOCKED LIMIT 1",
        )
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(row) = row else {
            transaction.commit().await?;
            return Ok(None);
        };
        let id: i64 = row.try_get("id")?;
        let attempts: i32 = row.try_get("attempts")?;
        let lease_until = Utc::now() + chrono::Duration::seconds(lease_seconds.max(1));
        sqlx::query(
            "UPDATE restore_jobs
             SET state='RUNNING', worker_id=$1, lease_until=$2,
                 attempts=attempts+1, updated_at=now()
             WHERE id=$3",
        )
        .bind(worker_id)
        .bind(lease_until)
        .bind(id)
        .execute(&mut *transaction)
        .await?;
        let mut job = restore_job_from_row(&row)?;
        job.state = "RUNNING".to_owned();
        job.worker_id = Some(worker_id.to_owned());
        job.lease_until = Some(lease_until);
        job.attempts = attempts + 1;
        transaction.commit().await?;
        Ok(Some(job))
    }

    pub async fn complete_restore_job(&self, job_id: i64) -> Result<(), DatabaseError> {
        sqlx::query(
            "UPDATE restore_jobs SET state='DONE', lease_until=NULL, worker_id=NULL,
                 last_error=NULL, updated_at=now() WHERE id=$1",
        )
        .bind(job_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn fail_restore_job(
        &self,
        job_id: i64,
        error: &str,
        retry: bool,
    ) -> Result<(), DatabaseError> {
        let state = if retry { "RETRY" } else { "FAILED" };
        sqlx::query(
            "UPDATE restore_jobs
             SET state=$1, lease_until=NULL, worker_id=NULL, last_error=$2,
                 next_attempt_at=now() + CASE WHEN $3 THEN interval '30 seconds'
                                               ELSE interval '0 seconds' END,
                 updated_at=now() WHERE id=$4",
        )
        .bind(state)
        .bind(error)
        .bind(retry)
        .bind(job_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn recover_expired_restore_jobs(&self) -> Result<u64, DatabaseError> {
        let result = sqlx::query(
            "UPDATE restore_jobs
             SET state=CASE WHEN attempts < max_attempts THEN 'RETRY' ELSE 'FAILED' END,
                 lease_until=NULL, worker_id=NULL, updated_at=now()
             WHERE state='RUNNING' AND lease_until IS NOT NULL AND lease_until < now()",
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    pub async fn list_restore_jobs(
        &self,
        states: Option<&[&str]>,
        limit: u32,
    ) -> Result<Vec<RestoreJob>, DatabaseError> {
        let rows = if let Some(states) = states {
            sqlx::query(
                "SELECT id, encoded_hash, target_provider, state, attempts, max_attempts,
                        next_attempt_at, lease_until, worker_id, last_error
                 FROM restore_jobs WHERE state = ANY($1)
                 ORDER BY updated_at DESC, id DESC LIMIT $2",
            )
            .bind(states)
            .bind(i64::from(limit.clamp(1, 500)))
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query(
                "SELECT id, encoded_hash, target_provider, state, attempts, max_attempts,
                        next_attempt_at, lease_until, worker_id, last_error
                 FROM restore_jobs ORDER BY updated_at DESC, id DESC LIMIT $1",
            )
            .bind(i64::from(limit.clamp(1, 500)))
            .fetch_all(&self.pool)
            .await?
        };
        rows.iter().map(restore_job_from_row).collect()
    }

    pub async fn restore_pending(&self, encoded_hash: &str) -> Result<bool, DatabaseError> {
        let row = sqlx::query(
            "SELECT EXISTS(
                SELECT 1 FROM restore_jobs
                WHERE encoded_hash=$1 AND state IN ('QUEUED','RUNNING','RETRY')
             ) AS pending",
        )
        .bind(encoded_hash)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.try_get("pending")?)
    }

    pub async fn claim_job(
        &self,
        worker_id: &str,
        lease_seconds: i64,
    ) -> Result<Option<ClaimedIngestionJob>, DatabaseError> {
        let mut transaction = self.pool.begin().await?;
        let row = sqlx::query("SELECT id, build_id, stage, attempts FROM ingestion_jobs WHERE attempts < max_attempts AND (lease_until IS NULL OR lease_until < now()) AND stage NOT IN ('DONE','FAILED') ORDER BY updated_at, id FOR UPDATE SKIP LOCKED LIMIT 1")
            .fetch_optional(&mut *transaction).await?;
        let Some(row) = row else {
            transaction.commit().await?;
            return Ok(None);
        };
        let id: i64 = row.try_get("id")?;
        let lease_until = Utc::now() + chrono::Duration::seconds(lease_seconds.max(1));
        let attempts: i32 = row.try_get("attempts")?;
        sqlx::query("UPDATE ingestion_jobs SET worker_id=$1, lease_until=$2, attempts=attempts+1, updated_at=now() WHERE id=$3")
            .bind(worker_id).bind(lease_until).bind(id).execute(&mut *transaction).await?;
        transaction.commit().await?;
        Ok(Some(ClaimedIngestionJob {
            id,
            build_id: row.try_get("build_id")?,
            stage: row.try_get("stage")?,
            attempts: attempts + 1,
            lease_until,
        }))
    }

    pub async fn complete_job(&self, job_id: i64, stage: &str) -> Result<(), DatabaseError> {
        sqlx::query("UPDATE ingestion_jobs SET stage=$1, lease_until=NULL, worker_id=NULL, last_error=NULL, updated_at=now() WHERE id=$2").bind(stage).bind(job_id).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn fail_job(
        &self,
        job_id: i64,
        error: &str,
        retry: bool,
    ) -> Result<(), DatabaseError> {
        let stage = if retry { "RETRY" } else { "FAILED" };
        sqlx::query("UPDATE ingestion_jobs SET stage=$1, lease_until=NULL, worker_id=NULL, last_error=$2, updated_at=now() WHERE id=$3").bind(stage).bind(error).bind(job_id).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn recover_expired_jobs(&self) -> Result<u64, DatabaseError> {
        let result = sqlx::query("UPDATE ingestion_jobs SET lease_until=NULL, worker_id=NULL, stage=CASE WHEN attempts < max_attempts THEN 'RETRY' ELSE 'FAILED' END, updated_at=now() WHERE lease_until IS NOT NULL AND lease_until < now() AND stage NOT IN ('DONE','FAILED')").execute(&self.pool).await?;
        Ok(result.rows_affected())
    }

    pub async fn get_manifest(&self, build_id: &str) -> Result<Option<Manifest>, DatabaseError> {
        let row =
            sqlx::query("SELECT manifest_json FROM builds WHERE id = $1 AND state = 'PUBLISHED'")
                .bind(build_id)
                .fetch_optional(&self.pool)
                .await?;
        row.map(|row| {
            let value: serde_json::Value = row.try_get("manifest_json")?;
            serde_json::from_value(value)
                .map_err(|error| DatabaseError::Manifest(error.to_string()))
        })
        .transpose()
    }

    pub async fn get_manifest_bytes(
        &self,
        build_id: &str,
    ) -> Result<Option<Vec<u8>>, DatabaseError> {
        let Some(row) =
            sqlx::query("SELECT manifest_bytes FROM builds WHERE id = $1 AND state = 'PUBLISHED'")
                .bind(build_id)
                .fetch_optional(&self.pool)
                .await?
        else {
            return Ok(None);
        };
        let bytes: Option<Vec<u8>> = row.try_get("manifest_bytes")?;
        Ok(bytes)
    }

    pub async fn get_signature(
        &self,
        build_id: &str,
    ) -> Result<Option<ManifestSignature>, DatabaseError> {
        let Some(row) =
            sqlx::query("SELECT signature_json FROM builds WHERE id = $1 AND state = 'PUBLISHED'")
                .bind(build_id)
                .fetch_optional(&self.pool)
                .await?
        else {
            return Ok(None);
        };
        let value: Option<serde_json::Value> = row.try_get("signature_json")?;
        value
            .map(|value| {
                serde_json::from_value(value)
                    .map_err(|error| DatabaseError::Manifest(error.to_string()))
            })
            .transpose()
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn close(&self) {
        self.pool.close().await;
    }
}

#[async_trait]
impl CapacityReservationStore for Database {
    async fn ensure_account(&self, account: StorageAccountSnapshot) -> Result<(), StorageError> {
        let capacity_bytes = u64_to_i64(account.capacity_bytes)
            .map_err(|error| StorageError::Configuration(error.to_string()))?;
        let safety_margin_bytes = u64_to_i64(account.safety_margin_bytes)
            .map_err(|error| StorageError::Configuration(error.to_string()))?;
        let pool_id = if account.pool_id.is_empty() {
            account.provider_id.clone()
        } else {
            account.pool_id.clone()
        };
        let failure_domain = if account.failure_domain.is_empty() {
            "mega".to_owned()
        } else {
            account.failure_domain.clone()
        };
        sqlx::query(
            "INSERT INTO storage_pools(
                id, storage_class, provider_type, priority, failure_domain,
                enabled, status, provisioning_mode
             ) VALUES($1,'COLD','mega',100,$2,TRUE,'READY','MANUAL')
             ON CONFLICT(id) DO NOTHING",
        )
        .bind(&pool_id)
        .bind(&failure_domain)
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        sqlx::query(
            "INSERT INTO storage_providers(id, kind, tier, pool_id)
             VALUES($1,'mega','COLD',$2) ON CONFLICT(id) DO UPDATE SET pool_id=excluded.pool_id",
        )
        .bind(&account.provider_id)
        .bind(&pool_id)
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        sqlx::query(
            "INSERT INTO storage_accounts(
                id, provider_id, pool_id, failure_domain, credential_reference, tier, status,
                capacity_bytes, safety_margin_bytes
             ) VALUES($1,$2,$3,$4,'operator-managed','COLD',$5,$6,$7)
             ON CONFLICT(id) DO UPDATE SET provider_id=excluded.provider_id,
                 pool_id=excluded.pool_id, failure_domain=excluded.failure_domain,
                 tier=excluded.tier, capacity_bytes=excluded.capacity_bytes,
                 safety_margin_bytes=excluded.safety_margin_bytes, updated_at=now()",
        )
        .bind(&account.account_id)
        .bind(&account.provider_id)
        .bind(&pool_id)
        .bind(&failure_domain)
        .bind(account.status.as_str())
        .bind(capacity_bytes)
        .bind(safety_margin_bytes)
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        Ok(())
    }

    async fn set_account_status(
        &self,
        account_id: &str,
        status: StorageAccountStatus,
    ) -> Result<(), StorageError> {
        Database::set_storage_account_status(self, account_id, status, None)
            .await
            .map_err(|error| storage_error(error.to_string()))
    }

    async fn refresh_account_capacity(
        &self,
        account_id: &str,
        snapshot: CapacitySnapshot,
    ) -> Result<StorageAccountSnapshot, StorageError> {
        let capacity_bytes = u64_to_i64(snapshot.capacity_bytes)
            .map_err(|error| StorageError::Configuration(error.to_string()))?;
        let used_bytes = u64_to_i64(snapshot.used_bytes)
            .map_err(|error| StorageError::Configuration(error.to_string()))?;
        let row = sqlx::query(
            "UPDATE storage_accounts
             SET capacity_bytes=$1, used_bytes=$2,
                 status=CASE
                     WHEN status='DISABLED' THEN 'DISABLED'
                     WHEN GREATEST(0::bigint, $1-$2-reserved_bytes-safety_margin_bytes)=0
                         THEN 'FULL'
                     WHEN GREATEST(0::bigint, $1-$2-reserved_bytes-safety_margin_bytes)
                          <= safety_margin_bytes*2 THEN 'NEAR_FULL'
                     ELSE 'ACTIVE'
                 END,
                 last_capacity_check=now(), updated_at=now()
             WHERE id=$3
             RETURNING id, provider_id, pool_id, failure_domain, tier, status,
                       capacity_bytes, used_bytes, reserved_bytes, safety_margin_bytes,
                       last_capacity_check",
        )
        .bind(capacity_bytes)
        .bind(used_bytes)
        .bind(account_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_error)?
        .ok_or_else(|| {
            StorageError::Configuration(format!("unknown storage account {account_id}"))
        })?;
        account_snapshot_from_row(&row).map_err(|error| storage_error(error.to_string()))
    }

    async fn list_accounts(
        &self,
        provider_id: &str,
    ) -> Result<Vec<StorageAccountSnapshot>, StorageError> {
        let rows = sqlx::query(
            "SELECT id, provider_id,
                    COALESCE(pool_id, provider_id) AS pool_id,
                    COALESCE(failure_domain, pool_id, provider_id) AS failure_domain,
                    tier, status, capacity_bytes, used_bytes,
                    reserved_bytes, safety_margin_bytes, last_capacity_check
             FROM storage_accounts WHERE provider_id=$1 ORDER BY id",
        )
        .bind(provider_id)
        .fetch_all(&self.pool)
        .await
        .map_err(storage_error)?;
        rows.iter()
            .map(|row| {
                account_snapshot_from_row(row).map_err(|error| storage_error(error.to_string()))
            })
            .collect()
    }

    async fn reserve(
        &self,
        account_id: &str,
        encoded_hash: &str,
        bytes: u64,
        ttl: Duration,
    ) -> Result<StorageReservation, StorageError> {
        let bytes =
            u64_to_i64(bytes).map_err(|error| StorageError::Configuration(error.to_string()))?;
        let ttl = chrono::Duration::from_std(ttl)
            .map_err(|error| StorageError::Configuration(error.to_string()))?;
        let mut transaction = self.pool.begin().await.map_err(storage_error)?;
        let account = sqlx::query(
            "SELECT id, provider_id,
                    COALESCE(pool_id, provider_id) AS pool_id,
                    COALESCE(failure_domain, pool_id, provider_id) AS failure_domain,
                    tier, status, capacity_bytes, used_bytes,
                    reserved_bytes, safety_margin_bytes, last_capacity_check
             FROM storage_accounts WHERE id=$1 FOR UPDATE",
        )
        .bind(account_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage_error)?
        .ok_or_else(|| {
            StorageError::Configuration(format!("unknown storage account {account_id}"))
        })?;

        let expired_row = sqlx::query(
            "SELECT COALESCE(SUM(bytes),0)::bigint AS bytes
             FROM storage_reservations
             WHERE account_id=$1 AND state='HELD' AND expires_at <= now()",
        )
        .bind(account_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(storage_error)?;
        let expired_bytes: i64 = expired_row.try_get("bytes").map_err(storage_error)?;
        if expired_bytes > 0 {
            sqlx::query(
                "UPDATE storage_reservations SET state='EXPIRED', updated_at=now()
                 WHERE account_id=$1 AND state='HELD' AND expires_at <= now()",
            )
            .bind(account_id)
            .execute(&mut *transaction)
            .await
            .map_err(storage_error)?;
            sqlx::query(
                "UPDATE storage_accounts
                 SET reserved_bytes=GREATEST(0::bigint, reserved_bytes-$1), updated_at=now()
                 WHERE id=$2",
            )
            .bind(expired_bytes)
            .bind(account_id)
            .execute(&mut *transaction)
            .await
            .map_err(storage_error)?;
        }

        if let Some(row) = sqlx::query(
            "SELECT id, expires_at, bytes FROM storage_reservations
             WHERE account_id=$1 AND encoded_hash=$2 AND state IN ('HELD','COMMITTED')
             ORDER BY id LIMIT 1",
        )
        .bind(account_id)
        .bind(encoded_hash)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage_error)?
        {
            let reservation_id: Uuid = row.try_get("id").map_err(storage_error)?;
            let expires_at: DateTime<Utc> = row.try_get("expires_at").map_err(storage_error)?;
            let existing_bytes: i64 = row.try_get("bytes").map_err(storage_error)?;
            transaction.commit().await.map_err(storage_error)?;
            return Ok(StorageReservation {
                reservation_id: reservation_id.to_string(),
                account_id: account_id.to_owned(),
                encoded_hash: encoded_hash.to_owned(),
                bytes: u64::try_from(existing_bytes).map_err(|_| {
                    StorageError::Configuration("negative reservation bytes".to_owned())
                })?,
                expires_at,
            });
        }

        let capacity_bytes: i64 = account.try_get("capacity_bytes").map_err(storage_error)?;
        let used_bytes: i64 = account.try_get("used_bytes").map_err(storage_error)?;
        let mut reserved_bytes: i64 = account.try_get("reserved_bytes").map_err(storage_error)?;
        if expired_bytes > 0 {
            reserved_bytes = reserved_bytes.saturating_sub(expired_bytes).max(0);
        }
        let safety_margin_bytes: i64 = account
            .try_get("safety_margin_bytes")
            .map_err(storage_error)?;
        let available = capacity_bytes
            .saturating_sub(used_bytes)
            .saturating_sub(reserved_bytes)
            .saturating_sub(safety_margin_bytes)
            .max(0);
        let status: String = account.try_get("status").map_err(storage_error)?;
        if !matches!(status.as_str(), "ACTIVE" | "NEAR_FULL") {
            if available < bytes {
                return Err(StorageError::NeedsCapacity {
                    required_bytes: u64::try_from(bytes).unwrap_or_default(),
                    available_bytes: u64::try_from(available).unwrap_or_default(),
                });
            }
            return Err(StorageError::Unavailable(format!(
                "storage account {account_id} is {status}"
            )));
        }
        if available < bytes {
            return Err(StorageError::NeedsCapacity {
                required_bytes: u64::try_from(bytes).unwrap_or_default(),
                available_bytes: u64::try_from(available).unwrap_or_default(),
            });
        }
        let reservation_id = Uuid::new_v4();
        let expires_at = Utc::now() + ttl;
        sqlx::query(
            "INSERT INTO storage_reservations(
                id, account_id, encoded_hash, bytes, state, expires_at
             ) VALUES($1,$2,$3,$4,'HELD',$5)",
        )
        .bind(reservation_id)
        .bind(account_id)
        .bind(encoded_hash)
        .bind(bytes)
        .bind(expires_at)
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        sqlx::query(
            "UPDATE storage_accounts SET reserved_bytes=reserved_bytes+$1, updated_at=now()
             WHERE id=$2",
        )
        .bind(bytes)
        .bind(account_id)
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(StorageReservation {
            reservation_id: reservation_id.to_string(),
            account_id: account_id.to_owned(),
            encoded_hash: encoded_hash.to_owned(),
            bytes: u64::try_from(bytes).map_err(|_| {
                StorageError::Configuration("negative reservation bytes".to_owned())
            })?,
            expires_at,
        })
    }

    async fn commit(&self, reservation_id: &str) -> Result<(), StorageError> {
        let reservation_id = Uuid::parse_str(reservation_id)
            .map_err(|error| StorageError::Configuration(error.to_string()))?;
        let mut transaction = self.pool.begin().await.map_err(storage_error)?;
        let row = sqlx::query(
            "SELECT account_id, bytes, state FROM storage_reservations
             WHERE id=$1 FOR UPDATE",
        )
        .bind(reservation_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage_error)?
        .ok_or_else(|| {
            StorageError::Configuration(format!("unknown reservation {reservation_id}"))
        })?;
        let state: String = row.try_get("state").map_err(storage_error)?;
        if state == "HELD" {
            let account_id: String = row.try_get("account_id").map_err(storage_error)?;
            let bytes: i64 = row.try_get("bytes").map_err(storage_error)?;
            sqlx::query(
                "UPDATE storage_reservations SET state='COMMITTED', updated_at=now() WHERE id=$1",
            )
            .bind(reservation_id)
            .execute(&mut *transaction)
            .await
            .map_err(storage_error)?;
            sqlx::query(
                "UPDATE storage_accounts
                 SET reserved_bytes=GREATEST(0::bigint, reserved_bytes-$1),
                     used_bytes=used_bytes+$1, updated_at=now()
                 WHERE id=$2",
            )
            .bind(bytes)
            .bind(account_id)
            .execute(&mut *transaction)
            .await
            .map_err(storage_error)?;
        }
        transaction.commit().await.map_err(storage_error)?;
        Ok(())
    }

    async fn release(&self, reservation_id: &str) -> Result<(), StorageError> {
        let reservation_id = Uuid::parse_str(reservation_id)
            .map_err(|error| StorageError::Configuration(error.to_string()))?;
        let mut transaction = self.pool.begin().await.map_err(storage_error)?;
        let row = sqlx::query(
            "SELECT account_id, bytes, state FROM storage_reservations
             WHERE id=$1 FOR UPDATE",
        )
        .bind(reservation_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage_error)?
        .ok_or_else(|| {
            StorageError::Configuration(format!("unknown reservation {reservation_id}"))
        })?;
        let state: String = row.try_get("state").map_err(storage_error)?;
        if state == "HELD" {
            let account_id: String = row.try_get("account_id").map_err(storage_error)?;
            let bytes: i64 = row.try_get("bytes").map_err(storage_error)?;
            sqlx::query(
                "UPDATE storage_reservations SET state='RELEASED', updated_at=now() WHERE id=$1",
            )
            .bind(reservation_id)
            .execute(&mut *transaction)
            .await
            .map_err(storage_error)?;
            sqlx::query(
                "UPDATE storage_accounts
                 SET reserved_bytes=GREATEST(0::bigint, reserved_bytes-$1), updated_at=now()
                 WHERE id=$2",
            )
            .bind(bytes)
            .bind(account_id)
            .execute(&mut *transaction)
            .await
            .map_err(storage_error)?;
        }
        transaction.commit().await.map_err(storage_error)?;
        Ok(())
    }

    async fn recover_expired(&self) -> Result<u64, StorageError> {
        let mut transaction = self.pool.begin().await.map_err(storage_error)?;
        let rows = sqlx::query(
            "SELECT id, account_id, bytes FROM storage_reservations
             WHERE state='HELD' AND expires_at <= now() FOR UPDATE",
        )
        .fetch_all(&mut *transaction)
        .await
        .map_err(storage_error)?;
        for row in &rows {
            let id: Uuid = row.try_get("id").map_err(storage_error)?;
            let account_id: String = row.try_get("account_id").map_err(storage_error)?;
            let bytes: i64 = row.try_get("bytes").map_err(storage_error)?;
            sqlx::query(
                "UPDATE storage_reservations SET state='EXPIRED', updated_at=now() WHERE id=$1",
            )
            .bind(id)
            .execute(&mut *transaction)
            .await
            .map_err(storage_error)?;
            sqlx::query(
                "UPDATE storage_accounts
                 SET reserved_bytes=GREATEST(0::bigint, reserved_bytes-$1), updated_at=now()
                 WHERE id=$2",
            )
            .bind(bytes)
            .bind(account_id)
            .execute(&mut *transaction)
            .await
            .map_err(storage_error)?;
        }
        transaction.commit().await.map_err(storage_error)?;
        Ok(rows.len() as u64)
    }
}
