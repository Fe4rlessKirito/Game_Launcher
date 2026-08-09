use chrono::{DateTime, Utc};
use launcher_common::{BuildSummary, CatalogPage, GameSummary, Manifest, ManifestSignature};
use sqlx::{PgPool, Row, postgres::PgPoolOptions};
use thiserror::Error;

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
        let migration = include_str!("../../../../migrations/001_initial.sql");
        sqlx::raw_sql(migration).execute(&self.pool).await?;
        Ok(())
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
        sqlx::query("INSERT INTO storage_locations(encoded_hash, provider, object_key, direct_url, priority, verified_at) VALUES($1,$2,$3,$4,$5,now()) ON CONFLICT(encoded_hash, provider, direct_url) DO UPDATE SET object_key=excluded.object_key, priority=excluded.priority, verified_at=now()")
            .bind(encoded_hash).bind(provider).bind(object_key).bind(direct_url).bind(priority).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn publish_build(&self, build_id: &str) -> Result<(), DatabaseError> {
        let mut transaction = self.pool.begin().await?;
        let row = sqlx::query("SELECT COUNT(*) AS missing FROM build_chunks bc LEFT JOIN storage_locations sl ON sl.encoded_hash = bc.encoded_hash AND sl.verified_at IS NOT NULL WHERE bc.build_id = $1 AND sl.encoded_hash IS NULL")
            .bind(build_id).fetch_one(&mut *transaction).await?;
        let missing: i64 = row.try_get("missing")?;
        if missing != 0 {
            return Err(DatabaseError::Manifest(format!(
                "cannot publish build {build_id}: {missing} chunk locations are unverified"
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
