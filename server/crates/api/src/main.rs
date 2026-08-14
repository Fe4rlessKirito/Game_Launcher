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
    collections::HashMap,
    env,
    net::SocketAddr,
    path::PathBuf,
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
    state: Arc<Mutex<RateWindow>>,
}

struct RateWindow {
    started: Instant,
    requests: u32,
    limit: u32,
    duration: Duration,
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
        Self::new(limit, Duration::from_secs(duration))
    }

    fn new(limit: u32, duration: Duration) -> Self {
        Self {
            state: Arc::new(Mutex::new(RateWindow {
                started: Instant::now(),
                requests: 0,
                limit: limit.max(1),
                duration: duration.max(Duration::from_secs(1)),
            })),
        }
    }

    fn retry_after_seconds(&self) -> Option<u64> {
        let mut state = self.state.lock().expect("rate limiter mutex poisoned");
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(state.started);
        if elapsed >= state.duration {
            state.started = now;
            state.requests = 0;
        }
        if state.requests < state.limit {
            state.requests += 1;
            return None;
        }
        Some(state.duration.saturating_sub(elapsed).as_secs().max(1))
    }
}

async fn enforce_rate_limit(
    State(rate_limiter): State<RequestRateLimiter>,
    request: Request,
    next: Next,
) -> Response {
    if let Some(retry_after) = rate_limiter.retry_after_seconds() {
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
    utc: chrono::DateTime<Utc>,
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
    };
    let app = Router::new()
        .route("/health", get(health))
        .route("/v1/health", get(liveness))
        .route("/v1/ready", get(readiness))
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
        utc: Utc::now(),
    }))
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
        let limiter = RequestRateLimiter::new(2, Duration::from_secs(1));
        assert!(limiter.retry_after_seconds().is_none());
        assert!(limiter.retry_after_seconds().is_none());
        assert!(limiter.retry_after_seconds().is_some());
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
