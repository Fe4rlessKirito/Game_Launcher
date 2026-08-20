use anyhow::Context;
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{Path, Query, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::Utc;
use launcher_common::{
    ApiErrorBody, BuildSummary, CatalogPage, ChunkResolutionRequest, GameSummary, HotPackSource,
    Manifest, ManifestSignature, PackResolutionRequest, ResolvedChunk, ResolvedPack,
    work_status::{WorkStatus, WorkStatusStore},
};
use launcher_database::Database;
use launcher_provisioning::{
    EmailIngestHeaders, FakeProvisioningEmailParser, FileSecretStore, MegaProvisioningEmailParser,
    ProvisioningEmailParserRegistry, ProvisioningError, ProvisioningMailRecord,
    ProvisioningManager, ProvisioningStatus, ProvisioningStore, manual_mega_provisioner,
    parse_mime, sha256_hex, verify_email_ingest,
};
use launcher_storage::{
    CapacityReservationStore, InMemoryCapacityReservationStore, LocalStorage, MirrorSet,
    StoragePolicy, StoragePool, StorageProvider, StorageProviderHealth, StorageRegistry,
    StorageTier, storage_from_env_with_reservation_store,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    env, fs,
    net::SocketAddr,
    path::PathBuf,
    process::Command,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::RwLock;
use tower::limit::ConcurrencyLimitLayer;
use tower_http::{cors::CorsLayer, limit::RequestBodyLimitLayer, trace::TraceLayer};
use tracing::{info, warn};
use uuid::Uuid;

const MIN_OPERATOR_TOKEN_BYTES: usize = 32;
const DEFAULT_RATE_LIMIT_REQUESTS: u32 = 600;
const DEFAULT_RATE_LIMIT_WINDOW_SECONDS: u64 = 60;

#[derive(Clone)]
struct RequestRateLimiter {
    state: Arc<Mutex<HashMap<String, RateWindow>>>,
    limit: u32,
    duration: Duration,
    trust_proxy_headers: bool,
}

struct RateWindow {
    started: Instant,
    requests: u32,
}

impl RequestRateLimiter {
    fn from_env() -> Self {
        let limit = env::var("LAUNCHER_RATE_LIMIT_REQUESTS")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(DEFAULT_RATE_LIMIT_REQUESTS)
            .clamp(1, 100_000);
        let duration = env::var("LAUNCHER_RATE_LIMIT_WINDOW_SECONDS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(DEFAULT_RATE_LIMIT_WINDOW_SECONDS)
            .clamp(1, 3_600);
        Self::with_proxy_headers(
            limit,
            Duration::from_secs(duration),
            env_bool("LAUNCHER_TRUST_PROXY_HEADERS", false),
        )
    }

    fn with_proxy_headers(limit: u32, duration: Duration, trust_proxy_headers: bool) -> Self {
        Self {
            state: Arc::new(Mutex::new(HashMap::new())),
            limit: limit.max(1),
            duration: duration.max(Duration::from_secs(1)),
            trust_proxy_headers,
        }
    }

    fn client_key(&self, headers: &HeaderMap) -> String {
        if self.trust_proxy_headers {
            for name in ["x-forwarded-for", "x-real-ip"] {
                if let Some(value) = headers.get(name).and_then(|value| value.to_str().ok()) {
                    let client = value.split(',').next().unwrap_or_default().trim();
                    if !client.is_empty() && client.len() <= 128 {
                        return format!("proxy:{client}");
                    }
                }
            }
        }
        "shared".to_owned()
    }

    fn retry_after_seconds(&self, client_key: &str) -> Option<u64> {
        const MAX_TRACKED_CLIENTS: usize = 10_000;
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => {
                warn!("rate limiter state lock was poisoned; resetting its in-memory windows");
                let mut state = poisoned.into_inner();
                state.clear();
                state
            }
        };
        let now = Instant::now();
        state.retain(|_, window| now.saturating_duration_since(window.started) < self.duration);
        if state.len() >= MAX_TRACKED_CLIENTS && !state.contains_key(client_key) {
            return Some(self.duration.as_secs().max(1));
        }
        let window = state.entry(client_key.to_owned()).or_insert(RateWindow {
            started: now,
            requests: 0,
        });
        if window.requests < self.limit {
            window.requests += 1;
            return None;
        }
        Some(
            self.duration
                .saturating_sub(now.saturating_duration_since(window.started))
                .as_secs()
                .max(1),
        )
    }
}

async fn enforce_rate_limit(
    State(rate_limiter): State<RequestRateLimiter>,
    request: Request,
    next: Next,
) -> Response {
    let client_key = rate_limiter.client_key(request.headers());
    if let Some(retry_after) = rate_limiter.retry_after_seconds(&client_key) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [(header::RETRY_AFTER, retry_after.to_string())],
            Json(ApiErrorBody {
                code: "rate_limited".to_owned(),
                message: "request rate limit exceeded".to_owned(),
                request_id: Uuid::new_v4().to_string(),
            }),
        )
            .into_response();
    }
    next.run(request).await
}

#[derive(Clone)]
struct AppState {
    database: Option<Database>,
    database_required: bool,
    storage: StorageRegistry,
    local_storage: Option<LocalStorage>,
    mirrors: MirrorSet,
    games: Arc<RwLock<Vec<GameSummary>>>,
    manifests: Arc<RwLock<HashMap<String, Manifest>>>,
    manifest_bytes: Arc<RwLock<HashMap<String, Vec<u8>>>>,
    signatures: Arc<RwLock<HashMap<String, ManifestSignature>>>,
    provisioning: Option<ProvisioningManager>,
    provisioning_enabled: bool,
    provisioning_email_domain: String,
    provisioning_email_hmac_secret: Option<Arc<Vec<u8>>>,
    provisioning_email_max_bytes: usize,
    provisioning_email_clock_skew_seconds: i64,
    packs_enabled: bool,
    public_base_url: String,
    cold_stream_worker_url: Option<String>,
    cold_stream_token: Option<Arc<String>>,
    cold_stream_client: reqwest::Client,
    operator_token: Option<Arc<String>>,
    operator_auth_required: bool,
    supabase_auth: Option<SupabaseAuth>,
    public_status: Arc<RwLock<PublicStatusResponse>>,
    public_status_poll_seconds: u64,
    work_status_store: WorkStatusStore,
    work_status_stale_seconds: u64,
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
    storage_providers: Vec<String>,
    storage_health: Vec<StorageProviderHealth>,
    utc: chrono::DateTime<Utc>,
}

#[derive(Debug, Serialize)]
struct LivenessResponse {
    status: &'static str,
    utc: chrono::DateTime<Utc>,
}

#[derive(Debug, Serialize)]
struct ReadinessResponse {
    status: &'static str,
    database_ready: bool,
    storage_configured: bool,
    provisioning_email_configured: bool,
    provisioning_enabled: bool,
    operator_auth_configured: bool,
    user_auth_configured: bool,
    utc: chrono::DateTime<Utc>,
}

#[derive(Clone)]
struct SupabaseAuth {
    user_endpoint: String,
    anon_key: Arc<String>,
    client: reqwest::Client,
}

#[derive(Debug, Deserialize)]
struct SupabaseUser {
    id: String,
    email: Option<String>,
    #[serde(default)]
    user_metadata: serde_json::Value,
}

impl SupabaseUser {
    fn username(&self) -> Option<String> {
        self.user_metadata
            .get("username")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty() && value.len() <= 24)
            .map(str::to_owned)
    }
}

#[derive(Debug)]
enum SupabaseAuthError {
    Unauthorized,
    Unavailable,
}

impl SupabaseAuth {
    fn from_env() -> anyhow::Result<Option<Self>> {
        let url = env::var("LAUNCHER_SUPABASE_URL")
            .ok()
            .map(|value| value.trim().trim_end_matches('/').to_owned())
            .filter(|value| !value.is_empty());
        let anon_key = env::var("LAUNCHER_SUPABASE_ANON_KEY")
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());

        match (url, anon_key) {
            (None, None) => Ok(None),
            (Some(url), Some(anon_key)) => {
                let parsed = reqwest::Url::parse(&url)
                    .with_context(|| "LAUNCHER_SUPABASE_URL must be a valid URL")?;
                if !matches!(parsed.scheme(), "http" | "https") {
                    anyhow::bail!("LAUNCHER_SUPABASE_URL must use http or https");
                }
                Ok(Some(Self {
                    user_endpoint: format!("{url}/auth/v1/user"),
                    anon_key: Arc::new(anon_key),
                    client: reqwest::Client::builder()
                        .build()
                        .context("could not create Supabase auth client")?,
                }))
            }
            _ => anyhow::bail!(
                "LAUNCHER_SUPABASE_URL and LAUNCHER_SUPABASE_ANON_KEY must be configured together"
            ),
        }
    }

    async fn authenticate(&self, token: &str) -> Result<SupabaseUser, SupabaseAuthError> {
        let response = self
            .client
            .get(&self.user_endpoint)
            .header("apikey", self.anon_key.as_str())
            .bearer_auth(token)
            .send()
            .await
            .map_err(|_| SupabaseAuthError::Unavailable)?;
        if response.status() == StatusCode::UNAUTHORIZED {
            return Err(SupabaseAuthError::Unauthorized);
        }
        if !response.status().is_success() {
            return Err(SupabaseAuthError::Unavailable);
        }
        response
            .json::<SupabaseUser>()
            .await
            .map_err(|_| SupabaseAuthError::Unavailable)
    }
}

#[derive(Debug, Serialize)]
struct CurrentUserResponse {
    id: String,
    email: Option<String>,
    username: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct PublicStatusResponse {
    status: &'static str,
    database_ready: bool,
    storage_configured: bool,
    providers: Vec<PublicProviderStatusResponse>,
    usage: Vec<PublicProviderUsageResponse>,
    total_used_bytes: u64,
    pending_restores: usize,
    active_work: Vec<PublicWorkStatusResponse>,
    system: PublicSystemMetrics,
    last_probe_at: Option<chrono::DateTime<Utc>>,
    last_successful_probe_at: Option<chrono::DateTime<Utc>>,
    probe_duration_ms: Option<u64>,
    probe_interval_seconds: u64,
    stale: bool,
    utc: chrono::DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
struct PublicWorkStatusResponse {
    id: String,
    kind: String,
    state: String,
    game: Option<String>,
    version: Option<String>,
    provider: Option<String>,
    source: Option<String>,
    detail: String,
    progress_percent: Option<f32>,
    bytes_completed: Option<u64>,
    bytes_total: Option<u64>,
    rate_bytes_per_second: Option<u64>,
    created_at: chrono::DateTime<Utc>,
    updated_at: chrono::DateTime<Utc>,
}

impl From<WorkStatus> for PublicWorkStatusResponse {
    fn from(status: WorkStatus) -> Self {
        Self {
            id: status.id,
            kind: status.kind,
            state: status.state,
            game: status.game,
            version: status.version,
            provider: status.provider,
            source: status.source,
            detail: status.detail,
            progress_percent: status.progress_percent,
            bytes_completed: status.bytes_completed,
            bytes_total: status.bytes_total,
            rate_bytes_per_second: status.rate_bytes_per_second,
            created_at: status.created_at,
            updated_at: status.updated_at,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct PublicSystemMetrics {
    cpu_usage_percent: Option<f64>,
    cpu_core_usage_percent: Option<Vec<f64>>,
    memory_used_bytes: Option<u64>,
    memory_total_bytes: Option<u64>,
    disk_used_bytes: Option<u64>,
    disk_total_bytes: Option<u64>,
    measured_at: chrono::DateTime<Utc>,
}

#[derive(Default)]
struct SystemProbeState {
    previous_cpu: Option<CpuCounters>,
    previous_cpu_cores: Vec<CpuCounters>,
}

#[derive(Clone, Copy)]
struct CpuCounters {
    total: u64,
    idle: u64,
}

struct CpuSnapshot {
    aggregate: CpuCounters,
    cores: Vec<CpuCounters>,
}

#[derive(Debug, Clone, Serialize)]
struct PublicProviderStatusResponse {
    provider: String,
    tier: StorageTier,
    healthy: bool,
}

#[derive(Debug, Clone, Serialize)]
struct PublicProviderUsageResponse {
    provider: String,
    tier: StorageTier,
    used_bytes: u64,
    last_capacity_check: Option<chrono::DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
struct EmailEventResponse {
    accepted: bool,
    duplicate: bool,
    job_id: Option<Uuid>,
}

#[derive(Debug, Serialize)]
struct StorageStatusResponse {
    policy: StoragePolicy,
    pools: Vec<StoragePool>,
    storage_health: Vec<StorageProviderHealth>,
    accounts: Vec<StorageAccountStatusResponse>,
    pending_restores: usize,
}

#[derive(Debug, Serialize)]
struct StorageAccountStatusResponse {
    account_id: String,
    provider_id: String,
    pool_id: String,
    failure_domain: String,
    tier: StorageTier,
    status: launcher_storage::StorageAccountStatus,
    capacity_bytes: u64,
    used_bytes: u64,
    reserved_bytes: u64,
    available_bytes: u64,
    last_capacity_check: Option<chrono::DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
struct StorageProviderCapabilityResponse {
    provider: String,
    pool_id: String,
    storage_class: StorageTier,
    provider_type: String,
    failure_domain: String,
    capabilities: launcher_storage::StorageProviderCapabilities,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let database_required = env::var("DATABASE_URL").is_ok();
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
    let provisioning_enabled = env_bool("PROVISIONING_ENABLED", false);
    let provisioning_email_domain =
        env::var("PROVISIONING_EMAIL_DOMAIN").unwrap_or_else(|_| "vaultnode.pp.ua".to_owned());
    let provisioning_email_hmac_secret = env::var("PROVISIONING_EMAIL_INGEST_HMAC_SECRET")
        .ok()
        .filter(|secret| !secret.is_empty())
        .map(|secret| Arc::new(secret.into_bytes()));
    let provisioning_email_max_bytes = env::var("PROVISIONING_EMAIL_MAX_BYTES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(5 * 1024 * 1024)
        .clamp(1024, 50 * 1024 * 1024);
    let provisioning_email_clock_skew_seconds =
        env::var("PROVISIONING_EMAIL_ALLOWED_CLOCK_SKEW_SECONDS")
            .ok()
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(300)
            .clamp(1, 3600);
    let packs_enabled = env_bool("PACK_STORAGE_ENABLED", false);
    let provisioning_alias_ttl = env::var("PROVISIONING_MAIL_ALIAS_TTL_SECONDS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(3600)
        .clamp(60, 86_400);
    let provisioning = database.as_ref().map(|database| {
        let secret_root = env::var("PROVISIONING_SECRET_STORE_DIR")
            .unwrap_or_else(|_| "provisioning-secrets".to_owned());
        let secret_store = Arc::new(FileSecretStore::new(secret_root));
        let mut registry = launcher_provisioning::ProvisionerRegistry::default();
        registry.register("mega", manual_mega_provisioner());
        if env_bool("PROVISIONING_ENABLE_FAKE", false) {
            registry.register_fake("fake", secret_store);
        }
        ProvisioningManager::new(Arc::new(database.clone()), registry).with_email_config(
            provisioning_email_domain.clone(),
            chrono::Duration::seconds(provisioning_alias_ttl),
        )
    });
    let storage_root = env::var_os("LAUNCHER_STORAGE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("storage"));
    let base_url =
        env::var("LAUNCHER_PUBLIC_BASE_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_owned());
    let cold_stream_worker_url = env::var("LAUNCHER_COLD_STREAM_WORKER_URL")
        .ok()
        .map(|value| value.trim_end_matches('/').to_owned())
        .filter(|value| !value.is_empty());
    let cold_stream_token = env::var("LAUNCHER_COLD_STREAM_TOKEN")
        .ok()
        .filter(|value| !value.is_empty())
        .map(Arc::new);
    let cold_stream_client = reqwest::Client::builder()
        .build()
        .context("could not create cold stream client")?;
    let operator_token = env::var("LAUNCHER_OPERATOR_TOKEN")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(|value| Arc::new(value.trim().to_owned()));
    if let Some(token) = operator_token.as_ref()
        && token.len() < MIN_OPERATOR_TOKEN_BYTES
    {
        anyhow::bail!("LAUNCHER_OPERATOR_TOKEN must be at least {MIN_OPERATOR_TOKEN_BYTES} bytes");
    }
    let operator_auth_required = env_bool("LAUNCHER_OPERATOR_AUTH_REQUIRED", false);
    let supabase_auth = SupabaseAuth::from_env()?;
    let max_request_bytes = env::var("LAUNCHER_MAX_REQUEST_BYTES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(50 * 1024 * 1024)
        .clamp(1024, 50 * 1024 * 1024);
    let max_concurrent_requests = env::var("LAUNCHER_MAX_CONCURRENT_REQUESTS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(256)
        .clamp(8, 4096);
    let rate_limiter = RequestRateLimiter::from_env();
    let cors = match env::var("LAUNCHER_CORS_ALLOW_ORIGIN")
        .ok()
        .filter(|value| !value.trim().is_empty())
    {
        Some(origin) => CorsLayer::new().allow_origin(
            origin
                .parse::<HeaderValue>()
                .context("LAUNCHER_CORS_ALLOW_ORIGIN must be a valid origin")?,
        ),
        None => CorsLayer::new(),
    };
    let reservation_store: Arc<dyn CapacityReservationStore> = database
        .as_ref()
        .map(|database| Arc::new(database.clone()) as Arc<dyn CapacityReservationStore>)
        .unwrap_or_else(|| Arc::new(InMemoryCapacityReservationStore::default()));
    let (storage, local_storage) =
        storage_from_env_with_reservation_store(&storage_root, &base_url, reservation_store)
            .await?;
    if let Some(database) = database.as_ref() {
        database.ensure_storage_pools(storage.pools()).await?;
    }
    let configured_mirrors = env::var("LAUNCHER_MIRROR_BASE_URLS").unwrap_or_default();
    let mirror_urls = configured_mirrors
        .split(',')
        .filter(|url| !url.trim().is_empty())
        .map(str::trim)
        .map(str::to_owned);
    let mirrors = MirrorSet::new(mirror_urls);
    let (games, manifests, manifest_bytes, signatures) = load_development_catalog();
    let public_status_poll_seconds = env::var("LAUNCHER_PUBLIC_STATUS_POLL_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(30)
        .clamp(10, 300);
    let work_status_dir = env::var_os("LAUNCHER_WORK_STATUS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| storage_root.join("work-status"));
    let work_status_stale_seconds = env::var("LAUNCHER_WORK_STATUS_STALE_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(900)
        .clamp(60, 86_400);
    let public_status_snapshot = Arc::new(RwLock::new(initial_public_status(
        !storage.providers().is_empty(),
        public_status_poll_seconds,
    )));
    let state = AppState {
        database,
        database_required,
        storage,
        local_storage,
        mirrors,
        games: Arc::new(RwLock::new(games)),
        manifests: Arc::new(RwLock::new(manifests)),
        manifest_bytes: Arc::new(RwLock::new(manifest_bytes)),
        signatures: Arc::new(RwLock::new(signatures)),
        provisioning,
        provisioning_enabled,
        provisioning_email_domain,
        provisioning_email_hmac_secret,
        provisioning_email_max_bytes,
        provisioning_email_clock_skew_seconds,
        packs_enabled,
        public_base_url: base_url,
        cold_stream_worker_url,
        cold_stream_token,
        cold_stream_client,
        operator_token,
        operator_auth_required,
        supabase_auth,
        public_status: public_status_snapshot,
        public_status_poll_seconds,
        work_status_store: WorkStatusStore::new(work_status_dir),
        work_status_stale_seconds,
    };
    tokio::spawn(run_public_status_monitor(state.clone()));
    let app = Router::new()
        .route("/health", get(health))
        .route("/v1/health", get(liveness))
        .route("/v1/ready", get(readiness))
        .route("/api/v1/public/status", get(public_status))
        .route("/api/v1/me", get(current_user))
        .route("/internal/v1/email-events", post(email_events))
        .route("/metrics", get(storage_metrics))
        .route("/api/v1/storage/status", get(storage_status))
        .route("/api/v1/storage/providers", get(storage_providers))
        .route("/api/v1/storage/metrics", get(storage_metrics))
        .route("/api/v1/games", get(list_games))
        .route("/api/v1/games/{id}", get(get_game))
        .route("/api/v1/builds/{id}/manifest", get(get_manifest))
        .route("/api/v1/builds/{id}/signature", get(get_signature))
        .route("/api/v1/builds/{id}/resolve", post(resolve_chunks))
        .route(
            "/api/v1/builds/{build_id}/chunks/{encoded_hash}",
            get(stream_hot_chunk),
        )
        .route("/api/v1/builds/{id}/packs/resolve", post(resolve_packs))
        .route(
            "/api/v1/builds/{build_id}/cold-packs/{pack_hash}",
            get(stream_cold_pack),
        )
        .route("/objects/{encoded_hash}", get(get_object))
        .route("/packs/{pack_hash}", get(get_pack))
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .layer(RequestBodyLimitLayer::new(max_request_bytes))
        .layer(ConcurrencyLimitLayer::new(max_concurrent_requests))
        .layer(middleware::from_fn_with_state(
            rate_limiter,
            enforce_rate_limit,
        ))
        .with_state(state);
    let address: SocketAddr = env::var("LAUNCHER_BIND")
        .or_else(|_| env::var("PORT").map(|port| format!("0.0.0.0:{port}")))
        .unwrap_or_else(|_| "127.0.0.1:8080".to_owned())
        .parse()?;
    info!(%address, "launcher API listening");
    let listener = tokio::net::TcpListener::bind(address).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

fn env_bool(name: &str, default: bool) -> bool {
    env::var(name)
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(default)
}

async fn email_events(
    State(state): State<AppState>,
    request: Request,
) -> Result<Json<EmailEventResponse>, ApiResponseError> {
    if !state.provisioning_enabled {
        return Err(ApiResponseError::temporary(
            "provisioning_disabled",
            "provisioning email ingest is disabled",
            60,
        ));
    }
    let Some(database) = &state.database else {
        return Err(ApiResponseError::temporary(
            "database_unavailable",
            "provisioning database is unavailable",
            15,
        ));
    };
    let Some(secret) = &state.provisioning_email_hmac_secret else {
        return Err(ApiResponseError::temporary(
            "email_ingest_unconfigured",
            "provisioning email ingest is not configured",
            60,
        ));
    };
    let (parts, body) = request.into_parts();
    if let Some(content_type) = parts
        .headers
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        && !content_type
            .split(';')
            .next()
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("message/rfc822"))
    {
        return Err(ApiResponseError::bad_request(
            "content type must be message/rfc822",
        ));
    }
    let body = to_bytes(body, state.provisioning_email_max_bytes.saturating_add(1))
        .await
        .map_err(|_| ApiResponseError::payload_too_large())?;
    if body.len() > state.provisioning_email_max_bytes {
        return Err(ApiResponseError::payload_too_large());
    }
    let header = |name: &str| {
        parts
            .headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    };
    let timestamp = header("x-mail-timestamp")
        .and_then(|value| value.parse::<i64>().ok())
        .ok_or_else(ApiResponseError::mail_authentication_failed)?;
    let nonce = header("x-mail-nonce").ok_or_else(ApiResponseError::mail_authentication_failed)?;
    let signature =
        header("x-mail-signature").ok_or_else(ApiResponseError::mail_authentication_failed)?;
    let envelope_to =
        header("x-envelope-to").ok_or_else(ApiResponseError::mail_authentication_failed)?;
    let envelope_from = header("x-envelope-from").filter(|value| !value.trim().is_empty());
    let ingest_headers = EmailIngestHeaders {
        timestamp,
        nonce,
        signature,
        envelope_from: envelope_from.clone(),
        envelope_to: envelope_to.trim().to_ascii_lowercase(),
    };
    let verification = verify_email_ingest(
        &ingest_headers,
        secret.as_slice(),
        &body,
        Utc::now(),
        chrono::Duration::seconds(state.provisioning_email_clock_skew_seconds),
    )
    .map_err(|_| ApiResponseError::mail_authentication_failed())?;
    let signed_at = chrono::DateTime::<Utc>::from_timestamp(verification.timestamp, 0)
        .ok_or_else(ApiResponseError::mail_authentication_failed)?;
    let nonce_expires_at =
        signed_at + chrono::Duration::seconds(state.provisioning_email_clock_skew_seconds);
    if !database
        .claim_mail_nonce(&verification.nonce, nonce_expires_at)
        .await
        .map_err(|_| ApiResponseError::internal("could not record mail nonce"))?
    {
        return Ok(Json(EmailEventResponse {
            accepted: false,
            duplicate: true,
            job_id: None,
        }));
    }
    let parsed = parse_mime(&body, state.provisioning_email_max_bytes)
        .map_err(|_| ApiResponseError::bad_request("invalid MIME message"))?;
    let Some(job) = database
        .find_active_job_by_email(&ingest_headers.envelope_to)
        .await
        .map_err(|_| ApiResponseError::internal("could not find provisioning email alias"))?
    else {
        // Deliberately do not disclose whether an alias was unknown, expired, or already closed.
        return Ok(Json(EmailEventResponse {
            accepted: false,
            duplicate: false,
            job_id: None,
        }));
    };
    let message_id = parsed
        .message_id
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("<{}@internal>", sha256_hex(&body)));
    let mut parsers = ProvisioningEmailParserRegistry::default();
    parsers.register(Box::new(FakeProvisioningEmailParser));
    parsers.register(Box::new(MegaProvisioningEmailParser));
    let event = match parsers.parse(&job.provider_type, &parsed) {
        Ok(event) => event,
        Err(_) => {
            return Ok(Json(EmailEventResponse {
                accepted: false,
                duplicate: false,
                job_id: None,
            }));
        }
    };
    let Some(provisioning) = &state.provisioning else {
        return Err(ApiResponseError::temporary(
            "provisioning_unavailable",
            "provisioning is not available",
            15,
        ));
    };
    let (updated, duplicate) = provisioning
        .handle_email_with_record_status(
            ProvisioningMailRecord {
                message_id,
                body_sha256: verification.body_sha256,
                envelope_from,
                envelope_to: ingest_headers.envelope_to,
                from_header: parsed.from,
                subject: parsed.subject,
                job_id: job.id,
            },
            event,
        )
        .await
        .map_err(|error| map_provisioning_error(error, "could not process provisioning email"))?;
    Ok(Json(EmailEventResponse {
        accepted: true,
        duplicate,
        job_id: Some(updated.id),
    }))
}

async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    let storage_health = state.storage.health().await;
    Json(HealthResponse {
        status: if storage_health.iter().all(|provider| provider.healthy) {
            "ok"
        } else {
            "degraded"
        },
        database_configured: state.database.is_some(),
        storage_providers: state
            .storage
            .providers()
            .iter()
            .map(|provider| provider.provider_id().to_owned())
            .collect(),
        storage_health,
        utc: Utc::now(),
    })
}

async fn liveness() -> Json<LivenessResponse> {
    Json(LivenessResponse {
        status: "ok",
        utc: Utc::now(),
    })
}

async fn readiness(
    State(state): State<AppState>,
) -> Result<Json<ReadinessResponse>, ApiResponseError> {
    let database_ready = if let Some(database) = &state.database {
        match database.ping().await {
            Ok(()) => true,
            Err(error) => {
                warn!(%error, "database readiness check failed");
                false
            }
        }
    } else {
        !state.database_required
    };
    let storage_configured = !state.storage.providers().is_empty();
    let provisioning_email_configured = !state.provisioning_enabled
        || (!state.provisioning_email_domain.trim().is_empty()
            && state.provisioning_email_hmac_secret.is_some());
    let operator_auth_configured = !state.operator_auth_required || state.operator_token.is_some();
    if !database_ready
        || !storage_configured
        || !provisioning_email_configured
        || !operator_auth_configured
    {
        return Err(ApiResponseError::temporary(
            "not_ready",
            if !database_ready {
                "database is not ready"
            } else if !storage_configured {
                "storage is not configured"
            } else {
                "operator authentication is not configured"
            },
            5,
        ));
    }
    Ok(Json(ReadinessResponse {
        status: "ready",
        database_ready,
        storage_configured,
        provisioning_email_configured,
        provisioning_enabled: state.provisioning_enabled,
        operator_auth_configured,
        user_auth_configured: state.supabase_auth.is_some(),
        utc: Utc::now(),
    }))
}

async fn current_user(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<CurrentUserResponse>, ApiResponseError> {
    let Some(auth) = &state.supabase_auth else {
        return Err(ApiResponseError::temporary(
            "user_auth_unconfigured",
            "user authentication is not configured",
            60,
        ));
    };
    let Some(token) = bearer_token(&headers) else {
        return Err(ApiResponseError::auth_required());
    };
    let user = match auth.authenticate(token).await {
        Ok(user) => user,
        Err(SupabaseAuthError::Unauthorized) => return Err(ApiResponseError::auth_required()),
        Err(SupabaseAuthError::Unavailable) => {
            return Err(ApiResponseError::temporary(
                "user_auth_unavailable",
                "user authentication service is unavailable",
                15,
            ));
        }
    };
    let username = user.username();
    Ok(Json(CurrentUserResponse {
        id: user.id,
        email: user.email,
        username,
    }))
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.len() <= 8192)
}

async fn storage_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<StorageStatusResponse>, ApiResponseError> {
    require_operator(&state, &headers)?;
    let policy = StoragePolicy::from_env().map_err(ApiResponseError::from)?;
    let accounts = if let Some(database) = &state.database {
        database
            .list_storage_accounts(None)
            .await
            .map_err(ApiResponseError::from)?
            .into_iter()
            .map(|record| {
                let snapshot = record.snapshot;
                let available_bytes = snapshot.usable_free_bytes();
                StorageAccountStatusResponse {
                    account_id: snapshot.account_id,
                    provider_id: snapshot.provider_id,
                    pool_id: snapshot.pool_id,
                    failure_domain: snapshot.failure_domain,
                    tier: snapshot.tier,
                    status: snapshot.status,
                    capacity_bytes: snapshot.capacity_bytes,
                    used_bytes: snapshot.used_bytes,
                    reserved_bytes: snapshot.reserved_bytes,
                    available_bytes,
                    last_capacity_check: snapshot.last_capacity_check,
                }
            })
            .collect()
    } else {
        Vec::new()
    };
    let pending_restores = if let Some(database) = &state.database {
        database
            .list_restore_jobs(Some(&["QUEUED", "RUNNING", "RETRY"]), 500)
            .await
            .map_err(ApiResponseError::from)?
            .len()
    } else {
        0
    };
    Ok(Json(StorageStatusResponse {
        policy,
        pools: state.storage.pools().to_vec(),
        storage_health: state.storage.health().await,
        accounts,
        pending_restores,
    }))
}

fn initial_public_status(storage_configured: bool, poll_seconds: u64) -> PublicStatusResponse {
    PublicStatusResponse {
        status: "starting",
        database_ready: false,
        storage_configured,
        providers: Vec::new(),
        usage: Vec::new(),
        total_used_bytes: 0,
        pending_restores: 0,
        active_work: Vec::new(),
        system: initial_system_metrics(),
        last_probe_at: None,
        last_successful_probe_at: None,
        probe_duration_ms: None,
        probe_interval_seconds: poll_seconds,
        stale: true,
        utc: Utc::now(),
    }
}

fn initial_system_metrics() -> PublicSystemMetrics {
    PublicSystemMetrics {
        cpu_usage_percent: None,
        cpu_core_usage_percent: None,
        memory_used_bytes: None,
        memory_total_bytes: None,
        disk_used_bytes: None,
        disk_total_bytes: None,
        measured_at: Utc::now(),
    }
}

fn sample_system_metrics(state: &mut SystemProbeState) -> PublicSystemMetrics {
    let (cpu_usage_percent, cpu_core_usage_percent) = match read_cpu_counters() {
        Some(current) => {
            let cpu_usage_percent = state
                .previous_cpu
                .and_then(|previous| calculate_cpu_usage_percent(current.aggregate, previous));
            let cpu_core_usage_percent = if !current.cores.is_empty()
                && current.cores.len() == state.previous_cpu_cores.len()
            {
                current
                    .cores
                    .iter()
                    .zip(state.previous_cpu_cores.iter())
                    .map(|(current, previous)| calculate_cpu_usage_percent(*current, *previous))
                    .collect()
            } else {
                None
            };
            state.previous_cpu = Some(current.aggregate);
            state.previous_cpu_cores = current.cores;
            (cpu_usage_percent, cpu_core_usage_percent)
        }
        None => (None, None),
    };
    let memory_total_bytes = proc_meminfo_bytes("MemTotal:");
    let memory_available_bytes = proc_meminfo_bytes("MemAvailable:");
    let memory_used_bytes = memory_total_bytes
        .zip(memory_available_bytes)
        .map(|(total, available)| total.saturating_sub(available));
    let (disk_used_bytes, disk_total_bytes) = read_root_disk_usage()
        .map(|(used, total)| (Some(used), Some(total)))
        .unwrap_or((None, None));
    PublicSystemMetrics {
        cpu_usage_percent,
        cpu_core_usage_percent,
        memory_used_bytes,
        memory_total_bytes,
        disk_used_bytes,
        disk_total_bytes,
        measured_at: Utc::now(),
    }
}

fn calculate_cpu_usage_percent(current: CpuCounters, previous: CpuCounters) -> Option<f64> {
    let total_delta = current.total.saturating_sub(previous.total);
    let idle_delta = current.idle.saturating_sub(previous.idle);
    (total_delta > 0).then(|| {
        ((total_delta.saturating_sub(idle_delta) as f64 / total_delta as f64) * 100.0)
            .clamp(0.0, 100.0)
    })
}

fn parse_cpu_counters(line: &str) -> Option<CpuCounters> {
    let values = line
        .split_whitespace()
        .skip(1)
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    let total = values.iter().copied().fold(0_u64, u64::saturating_add);
    let idle = values
        .get(3)
        .copied()
        .unwrap_or_default()
        .saturating_add(values.get(4).copied().unwrap_or_default());
    Some(CpuCounters { total, idle })
}

fn read_cpu_counters() -> Option<CpuSnapshot> {
    let contents = fs::read_to_string("/proc/stat").ok()?;
    let aggregate = contents
        .lines()
        .find(|line| line.starts_with("cpu "))
        .and_then(parse_cpu_counters)?;
    let mut cores = contents
        .lines()
        .filter_map(|line| {
            let label = line.split_whitespace().next()?;
            let index = label.strip_prefix("cpu")?.parse::<usize>().ok()?;
            Some((index, parse_cpu_counters(line)?))
        })
        .collect::<Vec<_>>();
    cores.sort_by_key(|(index, _)| *index);
    Some(CpuSnapshot {
        aggregate,
        cores: cores.into_iter().map(|(_, counters)| counters).collect(),
    })
}

fn proc_meminfo_bytes(label: &str) -> Option<u64> {
    let contents = fs::read_to_string("/proc/meminfo").ok()?;
    let line = contents.lines().find(|line| line.starts_with(label))?;
    let kilobytes = line.split_whitespace().nth(1)?.parse::<u64>().ok()?;
    Some(kilobytes.saturating_mul(1024))
}

fn read_root_disk_usage() -> Option<(u64, u64)> {
    let output = Command::new("df").args(["-B1", "-P", "/"]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let contents = String::from_utf8_lossy(&output.stdout);
    let line = contents.lines().skip(1).last()?;
    let fields = line.split_whitespace().collect::<Vec<_>>();
    let total = fields.get(1)?.parse::<u64>().ok()?;
    let used = fields.get(2)?.parse::<u64>().ok()?;
    Some((used, total))
}

async fn public_status(State(state): State<AppState>) -> Json<PublicStatusResponse> {
    Json(state.public_status.read().await.clone())
}

async fn run_public_status_monitor(state: AppState) {
    info!(
        poll_seconds = state.public_status_poll_seconds,
        "public telemetry monitor started"
    );
    let mut system_probe_state = SystemProbeState::default();
    loop {
        refresh_public_status(&state, &mut system_probe_state).await;
        tokio::time::sleep(Duration::from_secs(state.public_status_poll_seconds)).await;
    }
}

async fn refresh_public_status(state: &AppState, system_probe_state: &mut SystemProbeState) {
    let started = Instant::now();
    let probe_at = Utc::now();
    let previous = state.public_status.read().await.clone();
    let system = sample_system_metrics(system_probe_state);
    let providers = state
        .storage
        .health()
        .await
        .into_iter()
        .map(|provider| PublicProviderStatusResponse {
            provider: provider.provider,
            tier: provider.tier,
            healthy: provider.healthy,
        })
        .collect::<Vec<_>>();
    let mut stale = false;
    let mut database_ready = !state.database_required;
    let mut usage = previous.usage;
    let mut total_used_bytes = previous.total_used_bytes;
    let mut pending_restores = previous.pending_restores;
    let mut active_work = previous.active_work;

    if let Some(database) = &state.database {
        database_ready = match database.ping().await {
            Ok(()) => true,
            Err(error) => {
                stale = true;
                warn!(%error, "public telemetry database probe failed");
                false
            }
        };
        if database_ready {
            let mut usage_probe_ok = true;
            let mut usage_by_provider = HashMap::<String, PublicProviderUsageResponse>::new();
            match database.list_storage_accounts(None).await {
                Ok(accounts) => {
                    for account in accounts {
                        let snapshot = account.snapshot;
                        let entry = usage_by_provider
                            .entry(snapshot.provider_id.clone())
                            .or_insert_with(|| PublicProviderUsageResponse {
                                provider: snapshot.provider_id.clone(),
                                tier: snapshot.tier,
                                used_bytes: 0,
                                last_capacity_check: None,
                            });
                        entry.used_bytes = entry.used_bytes.saturating_add(snapshot.used_bytes);
                        if snapshot.last_capacity_check > entry.last_capacity_check {
                            entry.last_capacity_check = snapshot.last_capacity_check;
                        }
                    }
                }
                Err(error) => {
                    stale = true;
                    usage_probe_ok = false;
                    warn!(%error, "public telemetry storage usage probe failed");
                }
            }
            let renewal_days = env::var("LAUNCHER_HOT_RENEWAL_DAYS")
                .ok()
                .and_then(|value| value.parse::<i64>().ok())
                .unwrap_or(18)
                .clamp(1, 30);
            let mut pack_usage_providers = HashSet::new();
            match database
                .storage_pack_metrics(Utc::now() - chrono::Duration::days(renewal_days))
                .await
            {
                Ok(metrics) => {
                    for metric in metrics {
                        let used_bytes = u64::try_from(metric.used_bytes).unwrap_or_default();
                        let provider_id = metric.provider.clone();
                        let entry =
                            usage_by_provider
                                .entry(provider_id.clone())
                                .or_insert_with(|| PublicProviderUsageResponse {
                                    provider: provider_id.clone(),
                                    tier: metric.storage_class,
                                    used_bytes: 0,
                                    last_capacity_check: None,
                                });
                        // Physical-pack locations are the authoritative byte
                        // count for the providers used by Mantle. If a
                        // provider also has a capacity-account row, replace
                        // that broader account total with Vaultnode's tracked
                        // pack total instead of double-counting it.
                        if pack_usage_providers.insert(provider_id) {
                            entry.tier = metric.storage_class;
                            entry.used_bytes = used_bytes;
                        } else {
                            entry.used_bytes = entry.used_bytes.saturating_add(used_bytes);
                        }
                    }
                }
                Err(error) => {
                    stale = true;
                    usage_probe_ok = false;
                    warn!(%error, "public telemetry physical-pack usage probe failed");
                }
            }
            if usage_probe_ok {
                usage = usage_by_provider.into_values().collect();
                usage.sort_by(|left, right| left.provider.cmp(&right.provider));
                total_used_bytes = usage
                    .iter()
                    .map(|provider| provider.used_bytes)
                    .fold(0_u64, u64::saturating_add);
            }
            match database
                .list_restore_jobs(Some(&["QUEUED", "RUNNING", "RETRY"]), 500)
                .await
            {
                Ok(jobs) => pending_restores = jobs.len(),
                Err(error) => {
                    stale = true;
                    warn!(%error, "public telemetry restore queue probe failed");
                }
            }
        }
    }
    match state
        .work_status_store
        .read_active(Duration::from_secs(state.work_status_stale_seconds))
    {
        Ok(statuses) => {
            active_work = statuses
                .into_iter()
                .map(PublicWorkStatusResponse::from)
                .collect();
        }
        Err(error) => {
            stale = true;
            warn!(%error, "public telemetry work status probe failed");
        }
    }

    let storage_configured = !providers.is_empty();
    let status = if database_ready
        && storage_configured
        && !stale
        && providers.iter().all(|provider| provider.healthy)
    {
        "operational"
    } else {
        "degraded"
    };
    let last_successful_probe_at = if stale {
        previous.last_successful_probe_at
    } else {
        Some(probe_at)
    };
    let snapshot = PublicStatusResponse {
        status,
        database_ready,
        storage_configured,
        providers,
        usage,
        total_used_bytes,
        pending_restores,
        active_work,
        system,
        last_probe_at: Some(probe_at),
        last_successful_probe_at,
        probe_duration_ms: Some(started.elapsed().as_millis().min(u64::MAX as u128) as u64),
        probe_interval_seconds: state.public_status_poll_seconds,
        stale,
        utc: Utc::now(),
    };
    *state.public_status.write().await = snapshot;
}

async fn storage_providers(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<StorageProviderCapabilityResponse>>, ApiResponseError> {
    require_operator(&state, &headers)?;
    let mut providers = state
        .storage
        .providers()
        .iter()
        .filter_map(|provider| {
            let pool = state.storage.pool_for_provider(provider.provider_id())?;
            Some(StorageProviderCapabilityResponse {
                provider: provider.provider_id().to_owned(),
                pool_id: pool.id.clone(),
                storage_class: pool.storage_class,
                provider_type: pool.provider_type.clone(),
                failure_domain: pool.failure_domain.clone(),
                capabilities: provider.capabilities(),
            })
        })
        .collect::<Vec<_>>();
    providers.sort_by(|left, right| left.provider.cmp(&right.provider));
    Ok(Json(providers))
}

async fn storage_metrics(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiResponseError> {
    require_operator(&state, &headers)?;
    let policy = StoragePolicy::from_env().map_err(ApiResponseError::from)?;
    let health = state.storage.health().await;
    let accounts = if let Some(database) = &state.database {
        database
            .list_storage_accounts(None)
            .await
            .map_err(ApiResponseError::from)?
    } else {
        Vec::new()
    };
    let pending_restores = if let Some(database) = &state.database {
        database
            .list_restore_jobs(Some(&["QUEUED", "RUNNING", "RETRY"]), 500)
            .await
            .map_err(ApiResponseError::from)?
            .len()
    } else {
        0
    };
    let provisioning_jobs = if let Some(database) = &state.database {
        database
            .list_jobs(None, 500)
            .await
            .map_err(|_| ApiResponseError::internal("could not read provisioning metrics"))?
    } else {
        Vec::new()
    };
    let renewal_days = env::var("LAUNCHER_HOT_RENEWAL_DAYS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(18)
        .clamp(1, 30);
    let pack_metrics = if let Some(database) = &state.database {
        database
            .storage_pack_metrics(Utc::now() - chrono::Duration::days(renewal_days))
            .await
            .map_err(ApiResponseError::from)?
    } else {
        Vec::new()
    };
    let mut body = String::new();
    body.push_str(&format!(
        "launcher_storage_policy_min_hot_replicas {}\nlauncher_storage_policy_min_cold_replicas {}\nlauncher_storage_policy_min_archive_replicas {}\nlauncher_storage_policy_min_hot_failure_domains {}\nlauncher_storage_policy_min_cold_failure_domains {}\nlauncher_storage_policy_min_archive_failure_domains {}\nlauncher_storage_restore_pending {}\n",
        policy.min_verified_hot_replicas,
        policy.min_verified_cold_replicas,
        policy.min_verified_archive_replicas,
        policy.min_hot_failure_domains,
        policy.min_cold_failure_domains,
        policy.min_archive_failure_domains,
        pending_restores,
    ));
    body.push_str(&format!(
        "launcher_provisioning_enabled {}\nlauncher_provisioning_email_configured {}\n",
        if state.provisioning_enabled { 1 } else { 0 },
        if !state.provisioning_enabled || state.provisioning_email_hmac_secret.is_some() {
            1
        } else {
            0
        },
    ));
    body.push_str(&format!(
        "launcher_storage_hot_pack_renewal_window_seconds {}\n",
        renewal_days * 24 * 60 * 60
    ));
    for metric in &pack_metrics {
        let provider = metric_label(&metric.provider);
        let storage_class = metric.storage_class.as_str();
        body.push_str(&format!(
            "launcher_storage_pack_locations{{provider=\"{provider}\",storage_class=\"{storage_class}\"}} {}\n",
            metric.verified_locations
        ));
        body.push_str(&format!(
            "launcher_storage_pack_bytes{{provider=\"{provider}\",storage_class=\"{storage_class}\"}} {}\n",
            metric.used_bytes
        ));
        if metric.storage_class == StorageTier::Hot {
            body.push_str(&format!(
                "launcher_storage_hot_pack_renewal_due{{provider=\"{provider}\"}} {}\n",
                metric.renewal_due
            ));
        }
    }
    for status in [
        ProvisioningStatus::Created,
        ProvisioningStatus::Starting,
        ProvisioningStatus::RegistrationStarted,
        ProvisioningStatus::WaitingForEmail,
        ProvisioningStatus::EmailReceived,
        ProvisioningStatus::WaitingForProvider,
        ProvisioningStatus::CandidateReady,
        ProvisioningStatus::Validating,
        ProvisioningStatus::Enrolling,
        ProvisioningStatus::Enrolled,
        ProvisioningStatus::FailedRetryable,
        ProvisioningStatus::FailedPermanent,
        ProvisioningStatus::NeedsOperator,
        ProvisioningStatus::Cancelled,
    ] {
        let count = provisioning_jobs
            .iter()
            .filter(|job| job.status == status)
            .count();
        body.push_str(&format!(
            "launcher_provisioning_jobs{{status=\"{}\"}} {}\n",
            status.as_str(),
            count
        ));
    }
    for provider in &health {
        let label = metric_label(&provider.provider);
        let pool = metric_label(&provider.pool_id);
        let failure_domain = metric_label(&provider.failure_domain);
        let tier = provider.tier.as_str();
        body.push_str(&format!(
            "launcher_storage_provider_healthy{{provider=\"{label}\",pool=\"{pool}\",failure_domain=\"{failure_domain}\",tier=\"{tier}\"}} {}\n",
            if provider.healthy { 1 } else { 0 }
        ));
    }
    let mut pool_capacity = HashMap::<String, (u64, u64)>::new();
    for account in &accounts {
        let entry = pool_capacity
            .entry(account.snapshot.pool_id.clone())
            .or_default();
        entry.0 = entry.0.saturating_add(account.snapshot.usable_free_bytes());
        entry.1 = entry.1.saturating_add(account.snapshot.reserved_bytes);
    }
    for pool in state.storage.pools() {
        let pool_label = metric_label(&pool.id);
        let class = pool.storage_class.as_str();
        let domain = metric_label(&pool.failure_domain);
        let (free_bytes, reserved_bytes) = pool_capacity.get(&pool.id).copied().unwrap_or_default();
        body.push_str(&format!(
            "launcher_storage_pool_enabled{{pool=\"{pool_label}\",class=\"{class}\",failure_domain=\"{domain}\"}} {}\n",
            if pool.enabled { 1 } else { 0 }
        ));
        body.push_str(&format!(
            "launcher_storage_pool_free_bytes{{pool=\"{pool_label}\",class=\"{class}\"}} {free_bytes}\nlauncher_storage_pool_reserved_bytes{{pool=\"{pool_label}\",class=\"{class}\"}} {reserved_bytes}\n"
        ));
    }
    for class in [StorageTier::Hot, StorageTier::Cold, StorageTier::Archive] {
        let domains = health
            .iter()
            .filter(|provider| provider.storage_class == class && provider.healthy)
            .map(|provider| provider.failure_domain.as_str())
            .collect::<std::collections::HashSet<_>>()
            .len();
        let class_label = class.as_str();
        body.push_str(&format!(
            "launcher_storage_class_failure_domains{{class=\"{class_label}\"}} {domains}\n"
        ));
    }
    for account in accounts {
        let label = metric_label(&account.snapshot.account_id);
        body.push_str(&format!(
            "launcher_storage_account_available_bytes{{account=\"{label}\"}} {}\nlauncher_storage_account_reserved_bytes{{account=\"{label}\"}} {}\n",
            account.snapshot.usable_free_bytes(),
            account.snapshot.reserved_bytes,
        ));
    }
    Ok((
        StatusCode::OK,
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4",
        )],
        body,
    )
        .into_response())
}

fn require_operator(state: &AppState, headers: &HeaderMap) -> Result<(), ApiResponseError> {
    let configured = state.operator_token.is_some();
    if !configured && !state.operator_auth_required {
        return Ok(());
    }
    let Some(expected) = &state.operator_token else {
        return Err(ApiResponseError::temporary(
            "operator_auth_unconfigured",
            "operator authentication is required but not configured",
            60,
        ));
    };
    let supplied = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim);
    if supplied.is_none_or(|supplied| !constant_time_token_eq(supplied, expected)) {
        return Err(ApiResponseError::unauthorized());
    }
    Ok(())
}

fn constant_time_token_eq(supplied: &str, expected: &str) -> bool {
    let supplied = supplied.as_bytes();
    let expected = expected.as_bytes();
    let max_len = supplied.len().max(expected.len());
    let mut difference = supplied.len() ^ expected.len();

    for index in 0..max_len {
        let supplied_byte = supplied.get(index).copied().unwrap_or_default();
        let expected_byte = expected.get(index).copied().unwrap_or_default();
        difference |= usize::from(supplied_byte ^ expected_byte);
    }

    difference == 0
}

fn metric_label(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
                character
            } else {
                '_'
            }
        })
        .collect()
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
    let build_is_latest = if let Some(database) = &state.database {
        database
            .is_latest_published_build(&build_id)
            .await
            .map_err(ApiResponseError::from)?
    } else {
        true
    };
    let cold_stream_enabled = !build_is_latest
        && state.cold_stream_worker_url.is_some()
        && state.cold_stream_token.is_some();
    let database_locations = if let Some(database) = &state.database {
        database
            .get_storage_locations(&request.encoded_hashes)
            .await
            .map_err(ApiResponseError::from)?
    } else {
        HashMap::new()
    };
    let database_objects = if let Some(database) = &state.database {
        database
            .list_storage_objects(&request.encoded_hashes)
            .await
            .map_err(ApiResponseError::from)?
    } else {
        Vec::new()
    };
    let mut response = Vec::with_capacity(request.encoded_hashes.len());
    for hash in request.encoded_hashes {
        if !allowed.contains(&hash) {
            return Err(ApiResponseError::bad_request(
                "chunk is not referenced by the requested build",
            ));
        }
        let locations = if build_is_latest {
            state
                .storage
                .download_locations_for_tier(&hash, Some(StorageTier::Hot))
                .await
                .map_err(ApiResponseError::from)?
        } else {
            Vec::new()
        };
        let mirror_urls = state
            .mirrors
            .urls(&hash)
            .map_err(|error| ApiResponseError::bad_request(&error.to_string()))?;
        let mut urls = locations
            .iter()
            .map(|location| location.url.clone())
            .collect::<Vec<_>>();
        if build_is_latest {
            for record in database_locations
                .get(&hash)
                .into_iter()
                .flat_map(|records| records.iter())
                .filter(|record| record.tier == StorageTier::Hot && !record.direct_url.is_empty())
            {
                if !urls.contains(&record.direct_url) {
                    urls.push(record.direct_url.clone());
                }
            }
        }
        let has_cold_replica = database_locations
            .get(&hash)
            .into_iter()
            .flat_map(|records| records.iter())
            .any(|record| record.tier == StorageTier::Cold)
            || database_objects
                .iter()
                .any(|object| object.encoded_hash == hash && object.tier == StorageTier::Cold);
        if build_is_latest {
            for mirror_url in mirror_urls {
                if !urls.contains(&mirror_url) {
                    urls.push(mirror_url);
                }
            }
        }
        if build_is_latest
            && urls.is_empty()
            && state.packs_enabled
            && let Some(database) = &state.database
        {
            let hot_packs = database
                .get_hot_pack_sources_for_chunks(std::slice::from_ref(&hash))
                .await
                .map_err(ApiResponseError::from)?;
            let relay_available = hot_packs
                .get(&hash)
                .into_iter()
                .flat_map(|sources| sources.iter())
                .any(|source| {
                    let Some(provider) = state.storage.provider(&source.location.provider) else {
                        return false;
                    };
                    let Some(pool) = state.storage.pool_for_provider(provider.provider_id()) else {
                        return false;
                    };
                    pool.storage_class == StorageTier::Hot
                        && pool.enabled
                        && provider.capabilities().range_requests
                });
            if relay_available {
                urls.push(format!(
                    "{}/api/v1/builds/{}/chunks/{}",
                    state.public_base_url.trim_end_matches('/'),
                    build_id,
                    hash
                ));
            }
        }
        if !build_is_latest && urls.is_empty() && state.packs_enabled && cold_stream_enabled {
            if let Some(database) = &state.database {
                let cold_packs = database
                    .get_cold_pack_sources_for_build_chunks(&build_id, std::slice::from_ref(&hash))
                    .await
                    .map_err(ApiResponseError::from)?;
                if cold_packs.contains_key(&hash) {
                    // The pack resolver will expose the authenticated server
                    // stream. Do not enqueue or expose a historical HOT copy.
                    continue;
                }
            }
        } else if !build_is_latest
            && urls.is_empty()
            && state.packs_enabled
            && let Some(database) = &state.database
        {
            let hot_packs = database
                .get_hot_pack_sources_for_chunks(std::slice::from_ref(&hash))
                .await
                .map_err(ApiResponseError::from)?;
            if hot_packs.contains_key(&hash) {
                // The pack-first client path will resolve and materialize
                // the bytes. No logical HOT URL should be exposed for a
                // superseded build.
                continue;
            }
        }
        if urls.is_empty() {
            let cold_pack_hashes =
                if !cold_stream_enabled && !build_is_latest && state.packs_enabled {
                    if let Some(database) = &state.database {
                        database
                            .get_cold_pack_hashes_for_chunks(std::slice::from_ref(&hash))
                            .await
                            .map_err(ApiResponseError::from)?
                    } else {
                        HashMap::new()
                    }
                } else {
                    HashMap::new()
                };
            if let Some(database) = &state.database
                && (has_cold_replica || !cold_pack_hashes.is_empty())
            {
                for pack_hash in cold_pack_hashes.values().flatten() {
                    database
                        .enqueue_pack_restore_job(
                            pack_hash,
                            &env::var("LAUNCHER_RESTORE_TARGET_PROVIDER")
                                .unwrap_or_else(|_| "hot".to_owned()),
                        )
                        .await
                        .map_err(ApiResponseError::from)?;
                }
                if has_cold_replica {
                    database
                        .enqueue_restore_job(
                            &hash,
                            &env::var("LAUNCHER_RESTORE_TARGET_PROVIDER")
                                .unwrap_or_else(|_| "hot".to_owned()),
                        )
                        .await
                        .map_err(ApiResponseError::from)?;
                }
                return Err(ApiResponseError::temporary(
                    "restore_pending",
                    "the chunk is in cold storage and a hot restore has been queued",
                    30,
                ));
            }
            return Err(ApiResponseError {
                status: StatusCode::SERVICE_UNAVAILABLE,
                code: "no_chunk_locations",
                message: "no download locations are currently available".to_owned(),
                retry_after_seconds: None,
            });
        }
        let expires_at = locations
            .iter()
            .filter_map(|location| location.expires_at)
            .min();
        response.push(ResolvedChunk {
            encoded_hash: hash,
            urls,
            expires_at,
        });
    }
    Ok(Json(response))
}

async fn stream_hot_chunk(
    State(state): State<AppState>,
    Path((build_id, encoded_hash)): Path<(String, String)>,
) -> Result<Response, ApiResponseError> {
    if !state.packs_enabled {
        return Err(ApiResponseError::temporary(
            "pack_storage_disabled",
            "sparse chunk relay is disabled with physical pack storage disabled",
            60,
        ));
    }
    if encoded_hash.len() != 64
        || !encoded_hash
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(ApiResponseError::bad_request(
            "encoded hash must be 64 lowercase hexadecimal characters",
        ));
    }
    let Some(database) = &state.database else {
        return Err(ApiResponseError::temporary(
            "database_unavailable",
            "sparse chunk relay requires the metadata database",
            15,
        ));
    };
    let Some(manifest) = database
        .get_manifest(&build_id)
        .await
        .map_err(ApiResponseError::from)?
    else {
        return Err(ApiResponseError::not_found("build"));
    };
    if !manifest.files.iter().any(|file| {
        file.chunks
            .iter()
            .any(|chunk| chunk.encoded_hash == encoded_hash)
    }) {
        return Err(ApiResponseError::bad_request(
            "chunk is not referenced by the requested build",
        ));
    }
    if !database
        .is_latest_published_build(&build_id)
        .await
        .map_err(ApiResponseError::from)?
    {
        return Err(ApiResponseError::not_found("historical sparse chunk"));
    }

    let sources = database
        .get_hot_pack_sources_for_chunks(std::slice::from_ref(&encoded_hash))
        .await
        .map_err(ApiResponseError::from)?;
    let mut last_error = None;
    for source in sources
        .get(&encoded_hash)
        .into_iter()
        .flat_map(|items| items.iter())
    {
        let Some(provider) = state.storage.provider(&source.location.provider) else {
            continue;
        };
        let Some(pool) = state.storage.pool_for_provider(provider.provider_id()) else {
            continue;
        };
        if pool.storage_class != StorageTier::Hot
            || !pool.enabled
            || !provider.capabilities().range_requests
        {
            continue;
        }
        let Some(chunk) = database
            .get_pack_chunk(&source.pack_hash, &encoded_hash)
            .await
            .map_err(ApiResponseError::from)?
        else {
            continue;
        };
        let offset = match u64::try_from(chunk.encoded_offset) {
            Ok(value) => value,
            Err(_) => {
                last_error = Some("pack chunk offset is negative".to_owned());
                continue;
            }
        };
        let length = match u64::try_from(chunk.encoded_size) {
            Ok(value) => value,
            Err(_) => {
                last_error = Some("pack chunk size is negative".to_owned());
                continue;
            }
        };
        match provider
            .read_pack_range(&source.pack_hash, offset, length)
            .await
        {
            Ok(bytes)
                if bytes.len() as u64 == length
                    && blake3::hash(&bytes).to_hex().as_str() == encoded_hash =>
            {
                let response = Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "application/octet-stream")
                    .header(header::CONTENT_LENGTH, bytes.len().to_string())
                    .body(Body::from(bytes))
                    .map_err(|error| {
                        ApiResponseError::internal(&format!(
                            "could not build sparse chunk response: {error}"
                        ))
                    })?;
                return Ok(response);
            }
            Ok(bytes) => {
                last_error = Some(format!(
                    "provider {} returned an invalid sparse chunk ({} bytes)",
                    provider.provider_id(),
                    bytes.len()
                ));
            }
            Err(error) => last_error = Some(error.to_string()),
        }
    }
    Err(ApiResponseError::temporary(
        "sparse_relay_unavailable",
        &format!(
            "no verified HOT pack range is available for sparse chunk relay{}",
            last_error
                .as_deref()
                .map(|error| format!(": {error}"))
                .unwrap_or_default()
        ),
        15,
    ))
}

async fn resolve_packs(
    State(state): State<AppState>,
    Path(build_id): Path<String>,
    Json(request): Json<PackResolutionRequest>,
) -> Result<Json<Vec<ResolvedPack>>, ApiResponseError> {
    if !state.packs_enabled {
        return Err(ApiResponseError::temporary(
            "pack_storage_disabled",
            "physical pack resolution is disabled",
            60,
        ));
    }
    if request.encoded_hashes.is_empty() || request.encoded_hashes.len() > 512 {
        return Err(ApiResponseError::bad_request(
            "pack resolution requires between 1 and 512 chunks",
        ));
    }
    let Some(database) = &state.database else {
        return Err(ApiResponseError::temporary(
            "database_unavailable",
            "pack resolution requires the metadata database",
            15,
        ));
    };
    let allowed = database
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
        .ok_or_else(|| ApiResponseError::not_found("build"))?;
    if request
        .encoded_hashes
        .iter()
        .any(|encoded_hash| !allowed.contains(encoded_hash))
    {
        return Err(ApiResponseError::bad_request(
            "chunk is not referenced by the requested build",
        ));
    }

    let build_is_latest = database
        .is_latest_published_build(&build_id)
        .await
        .map_err(ApiResponseError::from)?;
    let cold_stream_enabled = !build_is_latest
        && state.cold_stream_worker_url.is_some()
        && state.cold_stream_token.is_some();

    let records = if build_is_latest {
        database
            .get_hot_pack_sources_for_chunks(&request.encoded_hashes)
            .await
            .map_err(ApiResponseError::from)?
    } else {
        HashMap::new()
    };
    let requested = request
        .encoded_hashes
        .into_iter()
        .collect::<std::collections::HashSet<_>>();
    let mut grouped = HashMap::<String, ResolvedPack>::new();
    for source in records.values().flatten() {
        let Some(provider) = state.storage.provider(&source.location.provider) else {
            continue;
        };
        let pool = state.storage.pool_for_provider(provider.provider_id());
        if pool.is_none_or(|pool| pool.storage_class != StorageTier::Hot || !pool.enabled)
            || !provider.capabilities().direct_download
        {
            continue;
        }
        let entry = grouped
            .entry(source.pack_hash.clone())
            .or_insert_with(|| ResolvedPack {
                pack_hash: source.pack_hash.clone(),
                encoded_size: source.encoded_size.max(0) as u64,
                chunk_hashes: source
                    .chunk_hashes
                    .iter()
                    .filter(|hash| requested.contains(*hash))
                    .cloned()
                    .collect(),
                sources: Vec::new(),
            });
        let runtime_location = provider
            .download_pack_location(&source.pack_hash)
            .await
            .ok();
        let url = runtime_location
            .as_ref()
            .map(|location| location.url.clone())
            .or_else(|| {
                (!source.location.direct_url.is_empty()).then(|| source.location.direct_url.clone())
            });
        let Some(url) = url else {
            continue;
        };
        let expires_at = runtime_location
            .as_ref()
            .and_then(|location| location.expires_at)
            .or(source.location.expires_at);
        if !entry
            .sources
            .iter()
            .any(|existing: &HotPackSource| existing.url == url)
        {
            let capabilities = provider.capabilities();
            entry.sources.push(HotPackSource {
                provider: provider.provider_id().to_owned(),
                pool_id: pool
                    .map(|pool| pool.id.clone())
                    .unwrap_or_else(|| source.location.pool_id.clone()),
                provider_type: provider.provider_type().to_owned(),
                failure_domain: pool
                    .map(|pool| pool.failure_domain.clone())
                    .unwrap_or_else(|| source.location.failure_domain.clone()),
                url,
                expires_at,
                range_supported: capabilities.range_requests,
                stable_url: capabilities.stable_urls,
                priority: source.location.priority,
            });
        }
    }
    if cold_stream_enabled {
        let cold_pack_sources = database
            .get_cold_pack_sources_for_build_chunks(
                &build_id,
                &requested.iter().cloned().collect::<Vec<_>>(),
            )
            .await
            .map_err(ApiResponseError::from)?;
        for (encoded_hash, packs) in cold_pack_sources {
            for (pack_hash, encoded_size) in packs {
                let entry = grouped
                    .entry(pack_hash.clone())
                    .or_insert_with(|| ResolvedPack {
                        pack_hash: pack_hash.clone(),
                        encoded_size: encoded_size.max(0) as u64,
                        chunk_hashes: Vec::new(),
                        sources: Vec::new(),
                    });
                if !entry.chunk_hashes.contains(&encoded_hash) {
                    entry.chunk_hashes.push(encoded_hash.clone());
                }
                let url = format!(
                    "{}/api/v1/builds/{}/cold-packs/{}",
                    state.public_base_url.trim_end_matches('/'),
                    build_id,
                    pack_hash
                );
                if !entry.sources.iter().any(|source| source.url == url) {
                    entry.sources.push(HotPackSource {
                        provider: "telegram-cold-stream".to_owned(),
                        pool_id: "telegram-cold".to_owned(),
                        provider_type: "telegram".to_owned(),
                        failure_domain: "telegram".to_owned(),
                        url,
                        expires_at: None,
                        range_supported: false,
                        stable_url: false,
                        priority: 1000,
                    });
                }
            }
        }
    }
    let cold_pack_hashes = if cold_stream_enabled {
        HashMap::new()
    } else {
        database
            .get_cold_pack_hashes_for_chunks(&requested.iter().cloned().collect::<Vec<_>>())
            .await
            .map_err(ApiResponseError::from)?
    };
    let mut queued_restore = false;
    let restore_target =
        env::var("LAUNCHER_RESTORE_TARGET_PROVIDER").unwrap_or_else(|_| "hot".to_owned());
    for pack_hashes in cold_pack_hashes.values() {
        for pack_hash in pack_hashes {
            if !grouped.contains_key(pack_hash) {
                database
                    .enqueue_pack_restore_job(pack_hash, &restore_target)
                    .await
                    .map_err(ApiResponseError::from)?;
                queued_restore = true;
            }
        }
    }
    if queued_restore {
        return Err(ApiResponseError::temporary(
            "restore_pending",
            "the requested pack is in COLD storage and a HOT restore has been queued",
            30,
        ));
    }
    let mut response = grouped
        .into_values()
        .filter(|pack| !pack.chunk_hashes.is_empty() && !pack.sources.is_empty())
        .collect::<Vec<_>>();
    response.sort_by(|left, right| left.pack_hash.cmp(&right.pack_hash));
    for pack in &mut response {
        pack.chunk_hashes.sort();
        pack.sources.sort_by(|left, right| {
            left.priority
                .cmp(&right.priority)
                .then_with(|| left.provider.cmp(&right.provider))
                .then_with(|| left.url.cmp(&right.url))
        });
    }
    Ok(Json(response))
}

async fn stream_cold_pack(
    State(state): State<AppState>,
    Path((build_id, pack_hash)): Path<(String, String)>,
    request: Request,
) -> Result<Response, ApiResponseError> {
    let Some(database) = &state.database else {
        return Err(ApiResponseError::temporary(
            "database_unavailable",
            "cold streaming requires the metadata database",
            15,
        ));
    };
    if database
        .is_latest_published_build(&build_id)
        .await
        .map_err(ApiResponseError::from)?
    {
        return Err(ApiResponseError::not_found("historical cold pack"));
    }
    if !database
        .cold_pack_available_for_build(&build_id, &pack_hash)
        .await
        .map_err(ApiResponseError::from)?
    {
        return Err(ApiResponseError::not_found("cold pack"));
    }
    let Some(worker_url) = &state.cold_stream_worker_url else {
        return Err(ApiResponseError::temporary(
            "cold_stream_unavailable",
            "the private cold stream worker is not configured",
            30,
        ));
    };
    let Some(token) = &state.cold_stream_token else {
        return Err(ApiResponseError::temporary(
            "cold_stream_unavailable",
            "the private cold stream worker token is not configured",
            30,
        ));
    };
    let worker_endpoint = format!(
        "{}/internal/v1/cold-packs/{}",
        worker_url.trim_end_matches('/'),
        pack_hash
    );
    let mut outbound = state
        .cold_stream_client
        .get(worker_endpoint)
        .bearer_auth(token.as_str());
    if let Some(range) = request.headers().get(header::RANGE) {
        outbound = outbound.header(header::RANGE, range);
    }
    let upstream = outbound.send().await.map_err(|error| {
        ApiResponseError::temporary(
            "cold_stream_unavailable",
            &format!("private cold stream worker unavailable: {error}"),
            30,
        )
    })?;
    let status =
        StatusCode::from_u16(upstream.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut response = Response::builder().status(status);
    for name in [
        header::CONTENT_TYPE,
        header::CONTENT_LENGTH,
        header::CONTENT_RANGE,
        header::ACCEPT_RANGES,
        header::RETRY_AFTER,
    ] {
        if let Some(value) = upstream.headers().get(&name) {
            response = response.header(name, value.clone());
        }
    }
    response
        .body(Body::from_stream(upstream.bytes_stream()))
        .map_err(|error| {
            ApiResponseError::internal(&format!("could not build cold stream: {error}"))
        })
}

async fn get_object(
    State(state): State<AppState>,
    Path(encoded_hash): Path<String>,
) -> Result<Response, ApiResponseError> {
    let local_storage = state
        .local_storage
        .as_ref()
        .ok_or_else(|| ApiResponseError::not_found("local object proxy"))?;
    if local_storage.tier() != StorageTier::Hot {
        return Err(ApiResponseError::not_found("hot object proxy"));
    }
    let bytes = local_storage
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

async fn get_pack(
    State(state): State<AppState>,
    Path(pack_hash): Path<String>,
) -> Result<Response, ApiResponseError> {
    let local_storage = state
        .local_storage
        .as_ref()
        .ok_or_else(|| ApiResponseError::not_found("local pack proxy"))?;
    if local_storage.tier() != StorageTier::Hot {
        return Err(ApiResponseError::not_found("hot pack proxy"));
    }
    let bytes = local_storage
        .read_pack(&pack_hash)
        .await
        .map_err(|error| ApiResponseError::not_found(&error.to_string()))?;
    Ok((
        StatusCode::OK,
        [
            (axum::http::header::CONTENT_TYPE, "application/octet-stream"),
            (axum::http::header::ACCEPT_RANGES, "bytes"),
        ],
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

#[derive(Debug)]
struct ApiResponseError {
    status: StatusCode,
    code: &'static str,
    message: String,
    retry_after_seconds: Option<u64>,
}

impl ApiResponseError {
    fn not_found(resource: &str) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: "not_found",
            message: format!("{resource} was not found"),
            retry_after_seconds: None,
        }
    }
    fn bad_request(message: &str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "bad_request",
            message: message.to_owned(),
            retry_after_seconds: None,
        }
    }

    fn temporary(code: &'static str, message: &str, retry_after_seconds: u64) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code,
            message: message.to_owned(),
            retry_after_seconds: Some(retry_after_seconds),
        }
    }

    fn payload_too_large() -> Self {
        Self {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            code: "payload_too_large",
            message: "request body exceeds the configured limit".to_owned(),
            retry_after_seconds: None,
        }
    }

    fn mail_authentication_failed() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            code: "mail_authentication_failed",
            message: "mail event authentication failed".to_owned(),
            retry_after_seconds: None,
        }
    }

    fn internal(message: &str) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "internal_error",
            message: message.to_owned(),
            retry_after_seconds: None,
        }
    }

    fn unauthorized() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            code: "operator_auth_required",
            message: "operator authentication required".to_owned(),
            retry_after_seconds: None,
        }
    }

    fn auth_required() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            code: "user_auth_required",
            message: "user authentication required".to_owned(),
            retry_after_seconds: None,
        }
    }
}

fn map_provisioning_error(error: ProvisioningError, fallback: &str) -> ApiResponseError {
    match error {
        ProvisioningError::Configuration(_) | ProvisioningError::Conflict(_) => {
            ApiResponseError::bad_request(fallback)
        }
        ProvisioningError::Security(_) => ApiResponseError::mail_authentication_failed(),
        ProvisioningError::NotFound => ApiResponseError::not_found("provisioning job"),
        ProvisioningError::Mail(_) => ApiResponseError::bad_request("invalid provisioning email"),
        ProvisioningError::Provider(_) | ProvisioningError::Secret(_) => {
            ApiResponseError::temporary("provisioning_unavailable", fallback, 15)
        }
        ProvisioningError::InvalidTransition { .. } => ApiResponseError::internal(fallback),
    }
}

impl From<launcher_database::DatabaseError> for ApiResponseError {
    fn from(error: launcher_database::DatabaseError) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "database_error",
            message: error.to_string(),
            retry_after_seconds: None,
        }
    }
}

impl From<launcher_storage::StorageError> for ApiResponseError {
    fn from(error: launcher_storage::StorageError) -> Self {
        let status = match error {
            launcher_storage::StorageError::Configuration(_)
            | launcher_storage::StorageError::InvalidHash => StatusCode::BAD_REQUEST,
            launcher_storage::StorageError::Provider(_)
            | launcher_storage::StorageError::RateLimiterClosed
            | launcher_storage::StorageError::NeedsCapacity { .. }
            | launcher_storage::StorageError::Authentication(_)
            | launcher_storage::StorageError::NetworkUnavailable(_)
            | launcher_storage::StorageError::Unavailable(_)
            | launcher_storage::StorageError::PoolUnavailable => StatusCode::SERVICE_UNAVAILABLE,
            launcher_storage::StorageError::Io(_)
            | launcher_storage::StorageError::Json(_)
            | launcher_storage::StorageError::HashMismatch { .. }
            | launcher_storage::StorageError::InjectedFailure => StatusCode::INTERNAL_SERVER_ERROR,
        };
        Self {
            status,
            code: "storage_error",
            message: error.to_string(),
            retry_after_seconds: None,
        }
    }
}

impl IntoResponse for ApiResponseError {
    fn into_response(self) -> Response {
        let mut response = (
            self.status,
            Json(ApiErrorBody {
                code: self.code.to_owned(),
                message: self.message,
                request_id: Uuid::new_v4().to_string(),
            }),
        )
            .into_response();
        if let Some(seconds) = self.retry_after_seconds
            && let Ok(value) = seconds.to_string().parse()
        {
            response.headers_mut().insert("retry-after", value);
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use launcher_common::{ChunkRef, ChunkingConfig, EncodingConfig, FileRecipe, LaunchProfile};

    #[test]
    fn cpu_usage_calculation_is_bounded() {
        let previous = CpuCounters {
            total: 1_000,
            idle: 700,
        };
        let current = CpuCounters {
            total: 1_200,
            idle: 800,
        };
        assert_eq!(calculate_cpu_usage_percent(current, previous), Some(50.0));
        assert_eq!(calculate_cpu_usage_percent(previous, current), None);
    }

    #[test]
    fn operator_token_comparison_requires_exact_bytes() {
        assert!(constant_time_token_eq("operator-token", "operator-token"));
        assert!(!constant_time_token_eq("operator-token", "operator-toke"));
        assert!(!constant_time_token_eq(
            "operator-token",
            "operator-token-extra"
        ));
        assert!(!constant_time_token_eq("", "operator-token"));
    }

    #[test]
    fn request_rate_limiter_rejects_after_budget() {
        let limiter = RequestRateLimiter::with_proxy_headers(2, Duration::from_secs(1), false);
        assert!(limiter.retry_after_seconds("client-a").is_none());
        assert!(limiter.retry_after_seconds("client-a").is_none());
        assert!(limiter.retry_after_seconds("client-a").is_some());
        assert!(limiter.retry_after_seconds("client-b").is_none());
    }

    #[test]
    fn trusted_proxy_rate_limiter_uses_the_first_forwarded_client() {
        let limiter = RequestRateLimiter::with_proxy_headers(1, Duration::from_secs(1), true);
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            "198.51.100.24, 10.0.0.2".parse().unwrap(),
        );
        assert_eq!(limiter.client_key(&headers), "proxy:198.51.100.24");
        assert!(
            limiter
                .retry_after_seconds(&limiter.client_key(&headers))
                .is_none()
        );
        assert!(
            limiter
                .retry_after_seconds(&limiter.client_key(&headers))
                .is_some()
        );
    }

    #[tokio::test]
    async fn resolve_chunks_returns_provider_and_independent_mirror_locations() {
        let bytes = b"alternate mirror fixture";
        let hash = "a".repeat(64);
        let manifest = Manifest {
            schema_version: 1,
            manifest_id: "manifest-1".to_owned(),
            game_id: "game-1".to_owned(),
            build_id: "build-1".to_owned(),
            display_version: "1.0.0".to_owned(),
            generated_at: Utc::now(),
            chunking: ChunkingConfig::default(),
            encoding: EncodingConfig::default(),
            files: vec![FileRecipe {
                path: "game.exe".to_owned(),
                size: bytes.len() as u64,
                blake3: hash.clone(),
                chunks: vec![ChunkRef {
                    raw_hash: hash.clone(),
                    raw_size: bytes.len() as u64,
                    encoded_hash: hash.clone(),
                    encoded_size: bytes.len() as u64,
                    object_key: format!("chunks/encoded/{hash}.bin"),
                }],
            }],
            launch: LaunchProfile {
                executable: "game.exe".to_owned(),
                working_directory: ".".to_owned(),
                ..LaunchProfile::default()
            },
        };
        let storage = StorageRegistry::new(vec![Arc::new(LocalStorage::new(
            "storage",
            "https://provider-a.example",
        ))])
        .unwrap();
        let state = AppState {
            database: None,
            database_required: false,
            storage,
            local_storage: None,
            mirrors: MirrorSet::new(["https://mirror-b.example", "https://mirror-c.example"]),
            games: Arc::new(RwLock::new(Vec::new())),
            manifests: Arc::new(RwLock::new(HashMap::from([(
                manifest.build_id.clone(),
                manifest,
            )]))),
            manifest_bytes: Arc::new(RwLock::new(HashMap::new())),
            signatures: Arc::new(RwLock::new(HashMap::new())),
            provisioning: None,
            provisioning_enabled: false,
            provisioning_email_domain: "vaultnode.pp.ua".to_owned(),
            provisioning_email_hmac_secret: None,
            provisioning_email_max_bytes: 5 * 1024 * 1024,
            provisioning_email_clock_skew_seconds: 300,
            packs_enabled: false,
            public_base_url: "https://launcher.example".to_owned(),
            cold_stream_worker_url: None,
            cold_stream_token: None,
            cold_stream_client: reqwest::Client::new(),
            operator_token: None,
            operator_auth_required: false,
            supabase_auth: None,
            public_status: Arc::new(RwLock::new(initial_public_status(true, 30))),
            public_status_poll_seconds: 30,
            work_status_store: WorkStatusStore::new("test-work-status"),
            work_status_stale_seconds: 900,
        };
        let resolved = resolve_chunks(
            State(state),
            Path("build-1".to_owned()),
            Json(ChunkResolutionRequest {
                encoded_hashes: vec![hash.clone()],
            }),
        )
        .await
        .unwrap()
        .0;

        assert_eq!(resolved.len(), 1);
        assert_eq!(
            resolved[0].urls,
            vec![
                format!("https://provider-a.example/objects/{hash}"),
                format!("https://mirror-b.example/objects/{hash}"),
                format!("https://mirror-c.example/objects/{hash}"),
            ]
        );
    }
}
