use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::Utc;
use launcher_common::{
    ApiErrorBody, BuildSummary, CatalogPage, ChunkResolutionRequest, GameSummary, Manifest,
    ManifestSignature, ResolvedChunk,
};
use launcher_database::Database;
use launcher_storage::{LocalStorage, MirrorSet, StorageProvider};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, env, net::SocketAddr, path::PathBuf, sync::Arc};
use tokio::sync::RwLock;
use tower_http::{cors::CorsLayer, trace::TraceLayer};
use tracing::{info, warn};
use uuid::Uuid;

#[derive(Clone)]
struct AppState {
    database: Option<Database>,
    storage: LocalStorage,
    mirrors: MirrorSet,
    games: Arc<RwLock<Vec<GameSummary>>>,
    manifests: Arc<RwLock<HashMap<String, Manifest>>>,
    manifest_bytes: Arc<RwLock<HashMap<String, Vec<u8>>>>,
    signatures: Arc<RwLock<HashMap<String, ManifestSignature>>>,
}

#[derive(Debug, Deserialize)]
struct CatalogQuery {
    cursor: Option<String>,
    limit: Option<u32>,
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: &'static str,
    database_configured: bool,
    utc: chrono::DateTime<Utc>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let storage_root = env::var_os("LAUNCHER_STORAGE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("storage"));
    let base_url =
        env::var("LAUNCHER_PUBLIC_BASE_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_owned());
    let storage = LocalStorage::new(storage_root, base_url.clone());
    let configured_mirrors = env::var("LAUNCHER_MIRROR_BASE_URLS").unwrap_or_default();
    let mirror_urls = configured_mirrors
        .split(',')
        .filter(|url| !url.trim().is_empty())
        .map(str::trim)
        .map(str::to_owned)
        .chain(std::iter::once(base_url.clone()));
    let mirrors = MirrorSet::new(mirror_urls);
    let database = match env::var("DATABASE_URL") {
        Ok(url) => match Database::connect(&url).await {
            Ok(database) => {
                if env::var("LAUNCHER_AUTO_MIGRATE").as_deref() == Ok("1") {
                    database.migrate().await?;
                }
                Some(database)
            }
            Err(error) => {
                warn!(%error, "database unavailable; using development catalog");
                None
            }
        },
        Err(_) => None,
    };
    let (games, manifests, manifest_bytes, signatures) = load_development_catalog();
    let state = AppState {
        database,
        storage,
        mirrors,
        games: Arc::new(RwLock::new(games)),
        manifests: Arc::new(RwLock::new(manifests)),
        manifest_bytes: Arc::new(RwLock::new(manifest_bytes)),
        signatures: Arc::new(RwLock::new(signatures)),
    };
    let app = Router::new()
        .route("/health", get(health))
        .route("/api/v1/games", get(list_games))
        .route("/api/v1/games/{id}", get(get_game))
        .route("/api/v1/builds/{id}/manifest", get(get_manifest))
        .route("/api/v1/builds/{id}/signature", get(get_signature))
        .route("/api/v1/builds/{id}/resolve", post(resolve_chunks))
        .route("/objects/{encoded_hash}", get(get_object))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state);
    let address: SocketAddr = env::var("LAUNCHER_BIND")
        .unwrap_or_else(|_| "127.0.0.1:8080".to_owned())
        .parse()?;
    info!(%address, "launcher API listening");
    let listener = tokio::net::TcpListener::bind(address).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        database_configured: state.database.is_some(),
        utc: Utc::now(),
    })
}

async fn list_games(
    State(state): State<AppState>,
    Query(query): Query<CatalogQuery>,
) -> Result<Json<CatalogPage>, ApiResponseError> {
    let limit = query.limit.unwrap_or(24).clamp(1, 100);
    let offset = query
        .cursor
        .as_deref()
        .unwrap_or("0")
        .parse::<u32>()
        .unwrap_or(0);
    if let Some(database) = &state.database {
        return Ok(Json(
            database
                .list_published_games(limit, offset)
                .await
                .map_err(ApiResponseError::from)?,
        ));
    }
    let games = state.games.read().await;
    let end = (offset as usize + limit as usize).min(games.len());
    let items = if (offset as usize) < games.len() {
        games[offset as usize..end].to_vec()
    } else {
        Vec::new()
    };
    let next_cursor = (end < games.len()).then(|| end.to_string());
    Ok(Json(CatalogPage { items, next_cursor }))
}

async fn get_game(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<GameSummary>, ApiResponseError> {
    if let Some(database) = &state.database {
        return database
            .get_game(&id)
            .await
            .map_err(ApiResponseError::from)?
            .map(Json)
            .ok_or_else(|| ApiResponseError::not_found("game"));
    }
    state
        .games
        .read()
        .await
        .iter()
        .find(|game| game.id == id || game.slug == id)
        .cloned()
        .map(Json)
        .ok_or_else(|| ApiResponseError::not_found("game"))
}

async fn get_manifest(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, ApiResponseError> {
    if let Some(database) = &state.database {
        if let Some(bytes) = database
            .get_manifest_bytes(&id)
            .await
            .map_err(ApiResponseError::from)?
        {
            return Ok((
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                bytes,
            )
                .into_response());
        }
        return database
            .get_manifest(&id)
            .await
            .map_err(ApiResponseError::from)?
            .map(|manifest| Json(manifest).into_response())
            .ok_or_else(|| ApiResponseError::not_found("manifest"));
    }
    let bytes = state
        .manifest_bytes
        .read()
        .await
        .get(&id)
        .cloned()
        .ok_or_else(|| ApiResponseError::not_found("manifest"))?;
    Ok((
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        bytes,
    )
        .into_response())
}

async fn get_signature(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ManifestSignature>, ApiResponseError> {
    if let Some(database) = &state.database {
        return database
            .get_signature(&id)
            .await
            .map_err(ApiResponseError::from)?
            .map(Json)
            .ok_or_else(|| ApiResponseError::not_found("manifest signature"));
    }
    state
        .signatures
        .read()
        .await
        .get(&id)
        .cloned()
        .map(Json)
        .ok_or_else(|| ApiResponseError::not_found("manifest signature"))
}

async fn resolve_chunks(
    State(state): State<AppState>,
    Path(build_id): Path<String>,
    Json(request): Json<ChunkResolutionRequest>,
) -> Result<Json<Vec<ResolvedChunk>>, ApiResponseError> {
    if request.encoded_hashes.len() > 512 {
        return Err(ApiResponseError::bad_request(
            "at most 512 chunks per resolution request",
        ));
    }
    let allowed = if let Some(database) = &state.database {
        database
            .get_manifest(&build_id)
            .await
            .map_err(ApiResponseError::from)?
            .map(|manifest| {
                manifest
                    .files
                    .into_iter()
                    .flat_map(|file| file.chunks.into_iter().map(|chunk| chunk.encoded_hash))
                    .collect::<std::collections::HashSet<_>>()
            })
    } else {
        state.manifests.read().await.get(&build_id).map(|manifest| {
            manifest
                .files
                .iter()
                .flat_map(|file| file.chunks.iter().map(|chunk| chunk.encoded_hash.clone()))
                .collect::<std::collections::HashSet<_>>()
        })
    };
    let allowed = allowed.ok_or_else(|| ApiResponseError::not_found("build"))?;
    let mut response = Vec::with_capacity(request.encoded_hashes.len());
    for hash in request.encoded_hashes {
        if !allowed.contains(&hash) {
            return Err(ApiResponseError::bad_request(
                "chunk is not referenced by the requested build",
            ));
        }
        let urls = state
            .mirrors
            .urls(&hash)
            .map_err(|error| ApiResponseError::bad_request(&error.to_string()))?;
        response.push(ResolvedChunk {
            encoded_hash: hash,
            urls,
            expires_at: None,
        });
    }
    Ok(Json(response))
}

async fn get_object(
    State(state): State<AppState>,
    Path(encoded_hash): Path<String>,
) -> Result<Response, ApiResponseError> {
    let bytes = state
        .storage
        .read_encoded(&encoded_hash)
        .await
        .map_err(|error| ApiResponseError::not_found(&error.to_string()))?;
    Ok((
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/octet-stream")],
        bytes,
    )
        .into_response())
}

type DevelopmentCatalog = (
    Vec<GameSummary>,
    HashMap<String, Manifest>,
    HashMap<String, Vec<u8>>,
    HashMap<String, ManifestSignature>,
);

fn load_development_catalog() -> DevelopmentCatalog {
    let mut manifests = HashMap::new();
    let mut manifest_bytes_by_build = HashMap::new();
    let mut signatures = HashMap::new();
    if let Ok(root) = env::var("LAUNCHER_CATALOG_ROOT") {
        let root = PathBuf::from(root);
        let mut games_by_id = HashMap::<String, GameSummary>::new();
        if let Ok(entries) = std::fs::read_dir(root) {
            for entry in entries.flatten() {
                let directory = entry.path();
                if !directory.is_dir() {
                    continue;
                }
                let manifest_path = directory.join("manifest.json");
                let signature_path = directory.join("manifest.sig.json");
                let Ok(manifest_bytes) = std::fs::read(&manifest_path) else {
                    continue;
                };
                let Ok(manifest) = serde_json::from_slice::<Manifest>(&manifest_bytes) else {
                    continue;
                };
                if manifest.validate().is_err() {
                    continue;
                }
                manifest_bytes_by_build.insert(manifest.build_id.clone(), manifest_bytes.clone());
                if let Ok(signature_bytes) = std::fs::read(&signature_path)
                    && let Ok(signature) =
                        serde_json::from_slice::<ManifestSignature>(&signature_bytes)
                {
                    signatures.insert(manifest.build_id.clone(), signature);
                }
                let build = BuildSummary {
                    id: manifest.build_id.clone(),
                    game_id: manifest.game_id.clone(),
                    display_version: manifest.display_version.clone(),
                    size_bytes: manifest.files.iter().map(|file| file.size).sum(),
                    published_at: Some(Utc::now()),
                };
                let game = GameSummary {
                    id: manifest.game_id.clone(),
                    slug: manifest.game_id.clone(),
                    title: "Synthetic Game".to_owned(),
                    description: "Published local synthetic build.".to_owned(),
                    hero_image_url: None,
                    cover_image_url: None,
                    latest_build: Some(build),
                };
                let replace = games_by_id
                    .get(&manifest.game_id)
                    .and_then(|existing| existing.latest_build.as_ref())
                    .map(|existing| {
                        game.latest_build
                            .as_ref()
                            .map(|latest| latest.id > existing.id)
                            .unwrap_or(false)
                    })
                    .unwrap_or(true);
                if replace {
                    games_by_id.insert(manifest.game_id.clone(), game);
                }
                manifests.insert(manifest.build_id.clone(), manifest);
            }
        }
        let mut games = games_by_id.into_values().collect::<Vec<_>>();
        games.sort_by(|a, b| a.id.cmp(&b.id));
        return (games, manifests, manifest_bytes_by_build, signatures);
    }
    if let Ok(path) = env::var("LAUNCHER_MANIFEST_PATH") {
        match std::fs::read(&path).and_then(|bytes| {
            serde_json::from_slice::<Manifest>(&bytes)
                .map(|manifest| (manifest, bytes))
                .map_err(std::io::Error::other)
        }) {
            Ok((manifest, manifest_bytes)) => {
                let build = BuildSummary {
                    id: manifest.build_id.clone(),
                    game_id: manifest.game_id.clone(),
                    display_version: manifest.display_version.clone(),
                    size_bytes: manifest.files.iter().map(|file| file.size).sum(),
                    published_at: Some(Utc::now()),
                };
                let game = GameSummary {
                    id: manifest.game_id.clone(),
                    slug: manifest.game_id.clone(),
                    title: "Synthetic Game".to_owned(),
                    description: "Development catalog entry generated from a local manifest."
                        .to_owned(),
                    hero_image_url: None,
                    cover_image_url: None,
                    latest_build: Some(build),
                };
                if let Ok(signature_path) = env::var("LAUNCHER_SIGNATURE_PATH")
                    && let Ok(signature_bytes) = std::fs::read(signature_path)
                    && let Ok(signature) =
                        serde_json::from_slice::<ManifestSignature>(&signature_bytes)
                {
                    signatures.insert(manifest.build_id.clone(), signature);
                }
                manifest_bytes_by_build.insert(manifest.build_id.clone(), manifest_bytes);
                manifests.insert(manifest.build_id.clone(), manifest);
                return (vec![game], manifests, manifest_bytes_by_build, signatures);
            }
            Err(error) => warn!(%error, "could not load LAUNCHER_MANIFEST_PATH"),
        }
    }
    (Vec::new(), manifests, manifest_bytes_by_build, signatures)
}

struct ApiResponseError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ApiResponseError {
    fn not_found(resource: &str) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: "not_found",
            message: format!("{resource} was not found"),
        }
    }
    fn bad_request(message: &str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "bad_request",
            message: message.to_owned(),
        }
    }
}

impl From<launcher_database::DatabaseError> for ApiResponseError {
    fn from(error: launcher_database::DatabaseError) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "database_error",
            message: error.to_string(),
        }
    }
}

impl IntoResponse for ApiResponseError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ApiErrorBody {
                code: self.code.to_owned(),
                message: self.message,
                request_id: Uuid::new_v4().to_string(),
            }),
        )
            .into_response()
    }
}
