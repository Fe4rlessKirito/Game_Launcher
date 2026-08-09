use chrono::{DateTime, Utc};
use launcher_common::{BuildSummary, CatalogPage, GameSummary, Manifest};
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

    pub async fn pool(&self) -> &PgPool {
        &self.pool
    }
}
