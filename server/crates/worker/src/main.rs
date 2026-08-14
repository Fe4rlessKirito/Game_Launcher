use anyhow::{Context, Result};
use async_trait::async_trait;
use axum::{
    Router,
    body::Body,
    extract::{Path as AxumPath, State as AxumState},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use chrono::Utc;
use clap::{Parser, Subcommand};
use futures_util::StreamExt;
use launcher_common::{ChunkRef, GameSummary, Manifest, ManifestSignature};
use launcher_database::Database;
use launcher_domain::BuildState;
use launcher_manifests::{
    generate_signing_key, load_private_key_pem, private_key_pem, public_key_pem, sign_bytes,
    validate_json, verify_bytes,
};
use launcher_normalizer::{NormalizationLimits, normalize_input};
use launcher_packager::{PackageOptions, package_directory};
use launcher_packs::PackConfig;
use launcher_provisioning::{
    CapacityCandidate, CapacityCandidateEnroller, CapacityCandidateValidator, FileSecretStore,
    ProvisionRequest, ProvisionerRegistry, ProvisioningError, ProvisioningManager,
    ProvisioningStatus, ProvisioningStore, SecretRef, ValidatedCapacity, manual_mega_provisioner,
};
use launcher_storage::{
    CapacityReservationStore, ExistingStorageReplica, InMemoryCapacityReservationStore,
    MegaAccountBackend, MegaAccountConfig, MegaCliAccount, MegaColdStorageConfig,
    StorageAccountStatus, StorageClass, StoragePlacementEngine, StoragePolicy, StorageProvider,
    StorageRegistry, StorageTier, storage_from_env_with_reservation_store,
};
use launcher_worker::IngestionProgress;
use rand::{RngCore, rngs::OsRng};
use std::process::Command;
use std::{
    collections::HashSet,
    env,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

#[derive(Debug, Parser)]
#[command(
    name = "launcher-admin",
    about = "Safe operator commands for authorized launcher content"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Clone)]
struct ColdStreamState {
    storage: StorageRegistry,
    token: Arc<String>,
}

async fn cold_pack_stream(
    AxumState(state): AxumState<ColdStreamState>,
    AxumPath(pack_hash): AxumPath<String>,
    headers: HeaderMap,
) -> Response {
    let expected = format!("Bearer {}", state.token);
    let authorized = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == expected);
    if !authorized {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let mut attempted = false;
    for provider in state.storage.restore_sources(StorageClass::Cold) {
        attempted = true;
        match provider.read_pack_stream(&pack_hash).await {
            Ok(stream) => {
                return Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "application/octet-stream")
                    .header(header::ACCEPT_RANGES, "none")
                    .body(Body::from_stream(stream))
                    .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
            }
            Err(_) => continue,
        }
    }
    if attempted {
        (StatusCode::BAD_GATEWAY, "cold pack source unavailable").into_response()
    } else {
        (StatusCode::NOT_FOUND, "no cold pack source configured").into_response()
    }
}

async fn run_cold_stream_server(bind: String, state: ColdStreamState) -> Result<()> {
    let app = Router::new()
        .route("/internal/v1/cold-packs/{pack_hash}", get(cold_pack_stream))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    println!("cold_stream=LISTENING bind={bind}");
    axum::serve(listener, app).await?;
    Ok(())
}

#[derive(Debug, Subcommand)]
enum Commands {
    Ingest {
        input: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long, default_value = "synthetic-game")]
        game_id: String,
        #[arg(long, default_value = "build-local")]
        build_id: String,
        #[arg(long, default_value = "0.1.0")]
        display_version: String,
        #[arg(long)]
        executable: Option<String>,
        #[arg(long, default_value_t = 1_048_576)]
        minimum_bytes: u64,
        #[arg(long, default_value_t = 4_194_304)]
        average_bytes: u64,
        #[arg(long, default_value_t = 16_777_216)]
        maximum_bytes: u64,
    },
    ManifestVerify {
        path: PathBuf,
    },
    HashFile {
        path: PathBuf,
    },
    ManifestSign {
        path: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long, default_value = "local-test-key")]
        key_id: String,
        #[arg(long)]
        private_key: Option<PathBuf>,
    },
    Publish {
        package: PathBuf,
        #[arg(long)]
        catalog_root: PathBuf,
        #[arg(long)]
        storage_root: PathBuf,
    },
    Db {
        #[command(subcommand)]
        command: DbCommands,
    },
    Signing {
        #[command(subcommand)]
        command: SigningCommands,
    },
    ConfigureStaging {
        #[arg(long)]
        api_url: String,
        #[arg(long)]
        public_key: PathBuf,
        #[arg(long, default_value = "staging-2026-01")]
        key_id: String,
        #[arg(long, default_value = "launcher-staging.json")]
        output: PathBuf,
        #[arg(long)]
        allow_http: bool,
        #[arg(long)]
        force: bool,
    },
    Staging {
        #[command(subcommand)]
        command: StagingCommands,
    },
    Storage {
        #[command(subcommand)]
        command: StorageCommands,
    },
    Provisioning {
        #[command(subcommand)]
        command: ProvisioningCommands,
    },
}

#[derive(Debug, Subcommand)]
enum DbCommands {
    Status,
    Migrate,
}

#[derive(Debug, Subcommand)]
enum SigningCommands {
    InitStaging {
        #[arg(long, default_value = "staging-keys")]
        output_dir: PathBuf,
        #[arg(long, default_value = "staging-2026-01")]
        key_id: String,
        #[arg(long)]
        force: bool,
    },
}

#[derive(Debug, Subcommand)]
enum StagingCommands {
    Verify {
        #[arg(long)]
        api_url: Option<String>,
        #[arg(long)]
        manifest_build_id: Option<String>,
        #[arg(long)]
        trusted_public_key: Option<PathBuf>,
        #[arg(long, default_value = "staging-2026-01")]
        expected_key_id: String,
        #[arg(long)]
        require_cold: bool,
        #[arg(long)]
        allow_http: bool,
    },
}

#[derive(Debug, Subcommand)]
enum StorageCommands {
    Policy,
    Pools {
        #[command(subcommand)]
        command: StoragePoolCommands,
    },
    Smoke {
        #[arg(long, default_value = "hot")]
        provider: String,
        #[arg(long, default_value = "storage")]
        storage_root: PathBuf,
        #[arg(long, default_value_t = 32 * 1024)]
        bytes: usize,
        #[arg(long)]
        skip_download_url: bool,
        #[arg(long)]
        upload_only: bool,
    },
    MegaSmoke {
        #[arg(long, default_value = "storage")]
        storage_root: PathBuf,
        #[arg(long, default_value_t = 32 * 1024)]
        bytes: usize,
    },
    TelegramSmoke {
        #[arg(long, default_value = "storage")]
        storage_root: PathBuf,
        #[arg(long, default_value_t = 1024 * 1024)]
        bytes: usize,
        #[arg(long, value_delimiter = ',', default_value = "1,2,4,8,16")]
        concurrency: Vec<usize>,
    },
    Health {
        #[arg(long, default_value = "storage")]
        storage_root: PathBuf,
    },
    Probe {
        #[arg(long, default_value = "hot")]
        provider: String,
        #[arg(long)]
        live: bool,
        #[arg(long, default_value = "storage")]
        storage_root: PathBuf,
    },
    Readiness {
        #[arg(long, default_value = "storage")]
        storage_root: PathBuf,
    },
    Accounts {
        #[command(subcommand)]
        command: StorageAccountCommands,
    },
    Restore {
        encoded_hash: String,
    },
    RestorePending {
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    Worker {
        #[arg(long, default_value = "launcher-restore-worker")]
        worker_id: String,
        #[arg(long, default_value_t = 15)]
        poll_seconds: u64,
        #[arg(long, default_value = "storage")]
        storage_root: PathBuf,
    },
    ColdRestoreSmoke {
        #[arg(long)]
        build_id: String,
        #[arg(long)]
        encoded_hash: String,
        #[arg(long, default_value = "hot")]
        target_provider: String,
        #[arg(long)]
        confirm: bool,
        #[arg(long, default_value = "storage")]
        storage_root: PathBuf,
    },
    ColdPackRestoreSmoke {
        #[arg(long)]
        build_id: String,
        #[arg(long)]
        pack_hash: String,
        #[arg(long, default_value = "filemirage")]
        target_provider: String,
        #[arg(long)]
        confirm: bool,
        #[arg(long)]
        metadata_only: bool,
        #[arg(long, default_value = "storage")]
        storage_root: PathBuf,
    },
    Gc {
        #[arg(long)]
        dry_run: bool,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
}

#[derive(Debug, Subcommand)]
enum StoragePoolCommands {
    List,
    Inspect { id: String },
}

#[derive(Debug, Subcommand)]
enum StorageAccountCommands {
    Add {
        #[arg(long)]
        account_id: String,
        #[arg(long)]
        credential_reference: String,
        #[arg(long)]
        home_dir: PathBuf,
        #[arg(long, default_value = "/launcher")]
        remote_root: String,
        #[arg(long, default_value = "")]
        command_dir: PathBuf,
        #[arg(long, default_value_t = 0)]
        capacity_bytes: u64,
        #[arg(long, default_value_t = 0)]
        safety_margin_bytes: u64,
        #[arg(long, default_value = "mega-cold")]
        provider_id: String,
    },
    List {
        #[arg(long)]
        provider_id: Option<String>,
    },
    Inspect {
        account_id: String,
    },
    Reauth {
        account_id: String,
    },
    Disable {
        account_id: String,
    },
}

#[derive(Debug, Subcommand)]
enum ProvisioningCommands {
    List {
        #[arg(long)]
        status: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    Inspect {
        job_id: String,
    },
    Retry {
        job_id: String,
    },
    Cancel {
        job_id: String,
        #[arg(long, default_value = "cancelled by operator")]
        reason: String,
    },
    CompleteManual {
        job_id: String,
        #[arg(long)]
        candidate_reference: String,
        #[arg(long)]
        credential_reference: String,
        #[arg(long)]
        expected_capacity_bytes: u64,
        #[arg(long, default_value = "mega")]
        provider_type: String,
    },
    Readiness,
    TestEmailAddress {
        address: Option<String>,
    },
    EnsureCapacity {
        #[arg(long)]
        provider_type: String,
        #[arg(long)]
        pool_id: String,
        #[arg(long)]
        requested_capacity_bytes: u64,
        #[arg(long)]
        idempotency_key: String,
        #[arg(long, default_value_t = 3600)]
        expires_seconds: i64,
    },
    Worker {
        #[arg(long, default_value_t = 15)]
        poll_seconds: u64,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Commands::ManifestVerify { path } => {
            let bytes = std::fs::read(&path)
                .with_context(|| format!("could not read {}", path.display()))?;
            let manifest = validate_json(&bytes).map_err(|error| anyhow::anyhow!(error))?;
            println!(
                "manifest={} game={} build={} files={} status=VALID",
                manifest.manifest_id,
                manifest.game_id,
                manifest.build_id,
                manifest.files.len()
            );
        }
        Commands::HashFile { path } => {
            let bytes = std::fs::read(&path)
                .with_context(|| format!("could not read {}", path.display()))?;
            println!(
                "blake3={} bytes={}",
                blake3::hash(&bytes).to_hex(),
                bytes.len()
            );
        }
        Commands::ManifestSign {
            path,
            output,
            key_id,
            private_key,
        } => {
            let bytes = std::fs::read(&path)
                .with_context(|| format!("could not read {}", path.display()))?;
            validate_json(&bytes).map_err(|error| anyhow::anyhow!(error))?;
            let private_key = private_key
                .or_else(|| env::var_os("LAUNCHER_SIGNING_PRIVATE_KEY_PATH").map(PathBuf::from));
            let key = match private_key {
                Some(path) => {
                    let pem = std::fs::read_to_string(&path)
                        .with_context(|| format!("could not read {}", path.display()))?;
                    load_private_key_pem(&pem)?
                }
                None => match env::var("LAUNCHER_SIGNING_PRIVATE_KEY_PEM") {
                    Ok(pem) => load_private_key_pem(&pem)?,
                    Err(_) if env_bool("LAUNCHER_SIGNING_REQUIRE_EXTERNAL_KEY", false) => {
                        anyhow::bail!(
                            "external signing key required; configure LAUNCHER_SIGNING_PRIVATE_KEY_PATH or LAUNCHER_SIGNING_PRIVATE_KEY_PEM"
                        )
                    }
                    Err(_) => generate_signing_key()?,
                },
            };
            let signature = sign_bytes(&bytes, key_id, &key, true)?;
            if let Some(parent) = output.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&output, serde_json::to_vec_pretty(&signature)?)?;
            println!("signature={} status=VALID", output.display());
        }
        Commands::Publish {
            package,
            catalog_root,
            storage_root,
        } => {
            let manifest_path = package.join("manifest.json");
            let signature_path = package.join("manifest.sig.json");
            let manifest_bytes = std::fs::read(&manifest_path)
                .with_context(|| format!("could not read {}", manifest_path.display()))?;
            let manifest =
                validate_json(&manifest_bytes).map_err(|error| anyhow::anyhow!(error))?;
            let signature_bytes = std::fs::read(&signature_path)
                .with_context(|| format!("could not read {}", signature_path.display()))?;
            let signature: ManifestSignature = serde_json::from_slice(&signature_bytes)?;
            let public_key = signature.public_key_base64.as_deref().ok_or_else(|| anyhow::anyhow!("local publish requires an embedded public key; production uses a trusted key ring"))?;
            let public_key = STANDARD.decode(public_key)?;
            verify_bytes(&manifest_bytes, &signature, &public_key)?;
            let destination = catalog_root.join(&manifest.build_id);
            std::fs::create_dir_all(&destination)?;
            std::fs::create_dir_all(storage_root.join("chunks/encoded"))?;
            copy_verified_objects(
                &package.join("chunks/encoded"),
                &storage_root.join("chunks/encoded"),
            )?;
            let database = if let Ok(url) = env::var("DATABASE_URL") {
                let database = Database::connect(&url).await?;
                if env::var("LAUNCHER_AUTO_MIGRATE").as_deref() == Ok("1") {
                    database.migrate().await?;
                }
                Some(database)
            } else {
                None
            };
            let base_url = env::var("LAUNCHER_PUBLIC_BASE_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:8080".to_owned());
            let reservation_store: Arc<dyn CapacityReservationStore> = database
                .as_ref()
                .map(|database| Arc::new(database.clone()) as Arc<dyn CapacityReservationStore>)
                .unwrap_or_else(|| Arc::new(InMemoryCapacityReservationStore::default()));
            let (storage, _) =
                storage_from_env_with_reservation_store(&storage_root, base_url, reservation_store)
                    .await?;
            publish_verified_build(
                &manifest,
                &manifest_bytes,
                &signature,
                &package,
                &storage,
                database.as_ref(),
            )
            .await?;
            atomic_copy(&manifest_path, &destination.join("manifest.json"))?;
            atomic_copy(&signature_path, &destination.join("manifest.sig.json"))?;
            println!(
                "publication=PUBLISHED game={} build={} catalog={} providers={:?} database={}",
                manifest.game_id,
                manifest.build_id,
                destination.display(),
                storage
                    .providers()
                    .iter()
                    .map(|provider| provider.provider_id())
                    .collect::<Vec<_>>(),
                database.is_some()
            );
        }
        Commands::Db { command } => handle_db_command(command).await?,
        Commands::Signing { command } => handle_signing_command(command)?,
        Commands::ConfigureStaging {
            api_url,
            public_key,
            key_id,
            output,
            allow_http,
            force,
        } => configure_staging(&api_url, &public_key, &key_id, &output, allow_http, force)?,
        Commands::Staging { command } => handle_staging_command(command).await?,
        Commands::Storage { command } => handle_storage_command(command).await?,
        Commands::Provisioning { command } => handle_provisioning_command(command).await?,
        Commands::Ingest {
            input,
            output,
            game_id,
            build_id,
            display_version,
            executable,
            minimum_bytes,
            average_bytes,
            maximum_bytes,
        } => {
            let mut progress = IngestionProgress::new();
            println!("stage={:?}", progress.state);
            std::fs::create_dir_all(&output)?;
            let normalized = normalize_input(&input, &NormalizationLimits::from_env()?)?;
            println!(
                "stage=NORMALIZED format={} root={}",
                normalized.format.as_str(),
                normalized.root.display()
            );
            let result = (|| -> Result<()> {
                let analysis_path = output.join("analysis.json");
                let status = Command::new("python")
                    .args(["-m", "launcher_analyzer", "analyze"])
                    .arg(&normalized.root)
                    .args(["--output"])
                    .arg(&analysis_path)
                    .status()
                    .context("could not start Python analyzer")?;
                if !status.success() {
                    anyhow::bail!("analyzer failed with status {status}");
                }
                progress.advance(BuildState::Analyzed)?;
                println!(
                    "stage={:?} report={}",
                    progress.state,
                    analysis_path.display()
                );
                let report = package_directory(
                    &normalized.root,
                    &output,
                    &PackageOptions {
                        game_id,
                        build_id,
                        display_version,
                        executable,
                        chunking: launcher_common::ChunkingConfig {
                            minimum_bytes,
                            average_bytes,
                            maximum_bytes,
                            ..launcher_common::ChunkingConfig::default()
                        },
                        pack_config: if env_bool("PACK_STORAGE_ENABLED", false) {
                            Some(PackConfig::from_env().map_err(|error| anyhow::anyhow!(error))?)
                        } else {
                            None
                        },
                        ..PackageOptions::default()
                    },
                )?;
                progress.advance(BuildState::Packaged)?;
                progress.advance(BuildState::Uploaded)?;
                progress.advance(BuildState::Verified)?;
                progress.advance(BuildState::Ready)?;
                println!(
                    "stage={:?} report={}",
                    progress.state,
                    serde_json::to_string_pretty(&report)?
                );
                println!("publication=EXPLICIT_OPERATOR_ACTION_REQUIRED");
                Ok(())
            })();
            normalized.cleanup()?;
            result?;
        }
    }
    Ok(())
}

async fn handle_db_command(command: DbCommands) -> Result<()> {
    match command {
        DbCommands::Status => {
            let database = connect_database().await?;
            let status = database.schema_status().await?;
            let tables = status
                .tables
                .iter()
                .map(|table| {
                    serde_json::json!({
                        "table": table.table,
                        "present": table.present,
                    })
                })
                .collect::<Vec<_>>();
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "database": "CONNECTED",
                    "schema_ready": status.ready(),
                    "tables": tables,
                }))?
            );
            if !status.ready() {
                anyhow::bail!("database schema is incomplete; run the controlled migration")
            }
        }
        DbCommands::Migrate => {
            let database = connect_database().await?;
            database.migrate().await?;
            println!("database=CONNECTED migration=APPLIED");
        }
    }
    Ok(())
}

fn handle_signing_command(command: SigningCommands) -> Result<()> {
    match command {
        SigningCommands::InitStaging {
            output_dir,
            key_id,
            force,
        } => init_staging_signing_key(&output_dir, &key_id, force),
    }
}

fn init_staging_signing_key(output_dir: &Path, key_id: &str, force: bool) -> Result<()> {
    validate_key_id(key_id)?;
    std::fs::create_dir_all(output_dir)?;
    let private_path = output_dir.join(format!("{key_id}.private.pem"));
    let public_path = output_dir.join(format!("{key_id}.public.pem"));
    if !force && (private_path.exists() || public_path.exists()) {
        anyhow::bail!(
            "refusing to overwrite existing staging key files; pass --force only after backing them up"
        );
    }
    let private_key = generate_signing_key()?;
    write_key_file(&private_path, &private_key_pem(&private_key)?, true, force)?;
    write_key_file(&public_path, &public_key_pem(&private_key)?, false, force)?;
    println!(
        "staging_signing=INITIALIZED key_id={key_id} private_key={} public_key={}",
        private_path.display(),
        public_path.display()
    );
    println!("private_key_output=DO_NOT_COMMIT_OR_SEND");
    Ok(())
}

fn configure_staging(
    api_url: &str,
    public_key_path: &Path,
    key_id: &str,
    output: &Path,
    allow_http: bool,
    force: bool,
) -> Result<()> {
    validate_key_id(key_id)?;
    let parsed = reqwest::Url::parse(api_url)
        .with_context(|| format!("invalid staging API URL: {api_url}"))?;
    if parsed.query().is_some() || parsed.fragment().is_some() {
        anyhow::bail!("staging API URL must not contain a query or fragment")
    }
    if parsed.scheme() != "https" && !allow_http {
        anyhow::bail!(
            "staging launcher configuration requires HTTPS; use --allow-http only for local smoke tests"
        )
    }
    let public_key = std::fs::read_to_string(public_key_path)
        .with_context(|| format!("could not read {}", public_key_path.display()))?;
    if !public_key.contains("BEGIN PUBLIC KEY") && !public_key.contains("BEGIN RSA PUBLIC KEY") {
        anyhow::bail!("public key must be PEM encoded")
    }
    if output.exists() && !force {
        anyhow::bail!(
            "refusing to overwrite {}; pass --force to replace it",
            output.display()
        )
    }
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let api_base_url = format!("{}/", api_url.trim_end_matches('/'));
    let config = serde_json::json!({
        "apiBaseUrl": api_base_url,
        "trustedManifestKeysPem": { key_id: public_key },
    });
    std::fs::write(output, serde_json::to_vec_pretty(&config)?)?;
    println!(
        "launcher_staging_config=WRITTEN output={} api_url={} key_id={key_id}",
        output.display(),
        api_base_url
    );
    Ok(())
}

fn validate_key_id(key_id: &str) -> Result<()> {
    if key_id.trim().is_empty()
        || !key_id.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
    {
        anyhow::bail!("key ID may contain only ASCII letters, digits, '-', '_', and '.'")
    }
    Ok(())
}

fn write_key_file(path: &Path, contents: &str, private: bool, force: bool) -> Result<()> {
    if path.exists() && !force {
        anyhow::bail!("refusing to overwrite {}", path.display())
    }
    std::fs::write(path, contents)?;
    if private {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
    }
    Ok(())
}

async fn handle_staging_command(command: StagingCommands) -> Result<()> {
    match command {
        StagingCommands::Verify {
            api_url,
            manifest_build_id,
            trusted_public_key,
            expected_key_id,
            require_cold,
            allow_http,
        } => {
            verify_staging(
                api_url,
                manifest_build_id,
                trusted_public_key,
                expected_key_id,
                require_cold,
                allow_http,
            )
            .await?;
        }
    }
    Ok(())
}

async fn verify_staging(
    api_url: Option<String>,
    manifest_build_id: Option<String>,
    trusted_public_key: Option<PathBuf>,
    expected_key_id: String,
    require_cold: bool,
    allow_http: bool,
) -> Result<()> {
    let raw_api_url = api_url
        .or_else(|| env::var("LAUNCHER_STAGING_API_URL").ok())
        .context("provide --api-url or LAUNCHER_STAGING_API_URL")?;
    let mut base_url = reqwest::Url::parse(&raw_api_url)
        .with_context(|| format!("invalid staging API URL: {raw_api_url}"))?;
    if base_url.query().is_some() || base_url.fragment().is_some() {
        anyhow::bail!("staging API URL must not contain a query or fragment")
    }
    if base_url.scheme() != "https" && !allow_http {
        anyhow::bail!(
            "staging verification requires an HTTPS API URL; use --allow-http only for local smoke tests"
        )
    }
    let root = base_url.path().trim_end_matches('/');
    let path = if root.is_empty() {
        "/".to_owned()
    } else {
        format!("{root}/")
    };
    base_url.set_path(&path);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?;
    let operator_token = env::var("LAUNCHER_OPERATOR_TOKEN")
        .ok()
        .filter(|value| !value.trim().is_empty());

    fetch_success(&client, &base_url.join("v1/health")?, "api_liveness").await?;
    fetch_success(&client, &base_url.join("v1/ready")?, "api_readiness").await?;
    let storage_status = fetch_json(
        &client,
        &base_url.join("api/v1/storage/status")?,
        "storage_status",
        operator_token.as_deref(),
    )
    .await?;
    fetch_success_with_auth(
        &client,
        &base_url.join("metrics")?,
        "metrics",
        operator_token.as_deref(),
    )
    .await?;

    let policy = storage_status
        .get("policy")
        .context("storage status did not return policy")?;
    let required_hot = policy
        .get("min_verified_hot_replicas")
        .and_then(serde_json::Value::as_u64)
        .context("storage policy is missing min_verified_hot_replicas")?;
    let required_cold = policy
        .get("min_verified_cold_replicas")
        .and_then(serde_json::Value::as_u64)
        .context("storage policy is missing min_verified_cold_replicas")?;
    let cold_backup_required = policy
        .get("cold_backup_required")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let health = storage_status
        .get("storage_health")
        .and_then(serde_json::Value::as_array)
        .context("storage status did not return storage_health")?;
    let healthy_count = |tier: &str| {
        health
            .iter()
            .filter(|provider| {
                provider.get("tier").and_then(serde_json::Value::as_str) == Some(tier)
                    && provider
                        .get("healthy")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false)
            })
            .count() as u64
    };
    let hot_healthy = healthy_count("HOT");
    let cold_provider_healthy = healthy_count("COLD");
    let cold_account_healthy = storage_status
        .get("accounts")
        .and_then(serde_json::Value::as_array)
        .map(|accounts| {
            accounts
                .iter()
                .filter(|account| {
                    account.get("tier").and_then(serde_json::Value::as_str) == Some("COLD")
                        && matches!(
                            account.get("status").and_then(serde_json::Value::as_str),
                            Some("ACTIVE" | "NEAR_FULL")
                        )
                        && account
                            .get("available_bytes")
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or(0)
                            > 0
                })
                .count() as u64
        })
        .unwrap_or(0);
    let cold_healthy = cold_provider_healthy.max(cold_account_healthy);
    if hot_healthy < required_hot {
        anyhow::bail!(
            "staging HOT policy is not satisfied: healthy={hot_healthy} required={required_hot}"
        )
    }
    if require_cold && (required_cold == 0 || !cold_backup_required) {
        anyhow::bail!("staging cold policy is not enabled")
    }
    if cold_healthy < required_cold || (require_cold && cold_healthy == 0) {
        anyhow::bail!(
            "staging COLD policy is not satisfied: healthy={cold_healthy} required={required_cold}"
        )
    }
    println!(
        "check=storage_policy status=PASS hot_healthy={hot_healthy} cold_healthy={cold_healthy} required_hot={required_hot} required_cold={required_cold}"
    );

    if let Some(build_id) = manifest_build_id {
        let public_key_path = trusted_public_key
            .context("--trusted-public-key is required when --manifest-build-id is used")?;
        let manifest_url = build_endpoint(&base_url, &build_id, "manifest")?;
        let manifest_response = client.get(manifest_url).send().await?;
        if !manifest_response.status().is_success() {
            anyhow::bail!(
                "staging manifest check failed: HTTP {}",
                manifest_response.status()
            )
        }
        let manifest_bytes = manifest_response.bytes().await?;
        let manifest = launcher_manifests::validate_json(&manifest_bytes)
            .map_err(|error| anyhow::anyhow!(error))?;
        let signature_url = build_endpoint(&base_url, &build_id, "signature")?;
        let signature: ManifestSignature = client
            .get(signature_url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        if signature.key_id != expected_key_id {
            anyhow::bail!(
                "staging signature key ID mismatch: expected {}, got {}",
                expected_key_id,
                signature.key_id
            )
        }
        let public_key = read_public_key_der(&public_key_path)?;
        verify_bytes(&manifest_bytes, &signature, &public_key)
            .map_err(|error| anyhow::anyhow!(error))?;
        println!(
            "check=signing status=PASS build={} key_id={} files={}",
            manifest.build_id,
            signature.key_id,
            manifest.files.len()
        );
    }
    println!("staging_verify=PASS");
    Ok(())
}

async fn fetch_success(client: &reqwest::Client, url: &reqwest::Url, name: &str) -> Result<()> {
    fetch_success_with_auth(client, url, name, None).await
}

async fn fetch_success_with_auth(
    client: &reqwest::Client,
    url: &reqwest::Url,
    name: &str,
    operator_token: Option<&str>,
) -> Result<()> {
    let request = if let Some(token) = operator_token {
        client.get(url.clone()).bearer_auth(token)
    } else {
        client.get(url.clone())
    };
    let response = request.send().await?;
    if !response.status().is_success() {
        if response.status() == reqwest::StatusCode::UNAUTHORIZED && operator_token.is_none() {
            anyhow::bail!(
                "staging check {name} requires LAUNCHER_OPERATOR_TOKEN in the verifier environment"
            )
        }
        anyhow::bail!("staging check {name} failed: HTTP {}", response.status())
    }
    println!("check={name} status=PASS http={}", response.status());
    Ok(())
}

async fn fetch_json(
    client: &reqwest::Client,
    url: &reqwest::Url,
    name: &str,
    operator_token: Option<&str>,
) -> Result<serde_json::Value> {
    let request = if let Some(token) = operator_token {
        client.get(url.clone()).bearer_auth(token)
    } else {
        client.get(url.clone())
    };
    let response = request.send().await?;
    if !response.status().is_success() {
        if response.status() == reqwest::StatusCode::UNAUTHORIZED && operator_token.is_none() {
            anyhow::bail!(
                "staging check {name} requires LAUNCHER_OPERATOR_TOKEN in the verifier environment"
            )
        }
        anyhow::bail!("staging check {name} failed: HTTP {}", response.status())
    }
    let status = response.status();
    let value = response.json().await?;
    println!("check={name} status=PASS http={status}");
    Ok(value)
}

fn read_public_key_der(path: &Path) -> Result<Vec<u8>> {
    let bytes =
        std::fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
    if bytes.starts_with(b"-----BEGIN") {
        let pem = String::from_utf8(bytes).context("trusted public key PEM is not UTF-8")?;
        let body = pem
            .lines()
            .filter(|line| !line.starts_with("-----"))
            .collect::<String>();
        return STANDARD
            .decode(body)
            .context("trusted public key PEM is not valid base64");
    }
    Ok(bytes)
}

fn build_endpoint(base_url: &reqwest::Url, build_id: &str, suffix: &str) -> Result<reqwest::Url> {
    // Keep the collection path without a trailing slash.  `push` on a
    // `Url` path-segment mutator preserves a trailing slash from the base,
    // which would produce `/manifest/` and miss the API's exact route.
    let mut url = base_url.join("api/v1/builds")?;
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| anyhow::anyhow!("staging API URL cannot be a base URL"))?;
        segments.push(build_id).push(suffix);
    }
    Ok(url)
}

async fn storage_command_context(
    storage_root: &Path,
) -> Result<(StorageRegistry, Option<Database>)> {
    let database = if let Ok(url) = env::var("DATABASE_URL") {
        let database = Database::connect(&url).await?;
        database.migrate().await?;
        Some(database)
    } else {
        None
    };
    let base_url =
        env::var("LAUNCHER_PUBLIC_BASE_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_owned());
    let reservation_store: Arc<dyn CapacityReservationStore> = database
        .as_ref()
        .map(|database| Arc::new(database.clone()) as Arc<dyn CapacityReservationStore>)
        .unwrap_or_else(|| Arc::new(InMemoryCapacityReservationStore::default()));
    let (storage, _) =
        storage_from_env_with_reservation_store(storage_root, base_url, reservation_store).await?;
    if let Some(database) = database.as_ref() {
        database.ensure_storage_pools(storage.pools()).await?;
    }
    Ok((storage, database))
}

async fn handle_storage_command(command: StorageCommands) -> Result<()> {
    match command {
        StorageCommands::Policy => {
            let policy = StoragePolicy::from_env().map_err(|error| anyhow::anyhow!(error))?;
            println!("{}", serde_json::to_string_pretty(&policy)?);
        }
        StorageCommands::Pools { command } => {
            let storage_root = env::var_os("LAUNCHER_STORAGE_ROOT")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("storage"));
            let (storage, _) = storage_command_context(&storage_root).await?;
            let health = storage.health().await;
            match command {
                StoragePoolCommands::List => {
                    for pool in storage.pools() {
                        let provider_health =
                            health.iter().find(|candidate| candidate.pool_id == pool.id);
                        println!(
                            "pool={} class={} provider_type={} priority={} failure_domain={} enabled={} status={} provisioning={} healthy={}",
                            pool.id,
                            pool.storage_class,
                            pool.provider_type,
                            pool.priority,
                            pool.failure_domain,
                            pool.enabled,
                            pool.status.as_str(),
                            pool.provisioning_mode,
                            provider_health.is_some_and(|candidate| candidate.healthy),
                        );
                    }
                }
                StoragePoolCommands::Inspect { id } => {
                    let pool = storage
                        .pool(&id)
                        .with_context(|| format!("storage pool {id:?} is not configured"))?;
                    println!("{}", serde_json::to_string_pretty(pool)?);
                    if let Some(provider_health) =
                        health.iter().find(|candidate| candidate.pool_id == id)
                    {
                        println!("health=");
                        println!("{}", serde_json::to_string_pretty(provider_health)?);
                    }
                }
            }
        }
        StorageCommands::Smoke {
            provider,
            storage_root,
            bytes,
            skip_download_url,
            upload_only,
        } => {
            let (storage, _) = storage_command_context(&storage_root).await?;
            let provider = select_provider(&storage, &provider, StorageTier::Hot)?;
            if provider.tier() != StorageTier::Hot {
                anyhow::bail!(
                    "storage smoke requires a HOT provider; use storage mega-smoke for COLD"
                )
            }
            run_storage_smoke(
                provider,
                bytes,
                !skip_download_url && !upload_only,
                upload_only,
                "HOT",
            )
            .await?;
        }
        StorageCommands::MegaSmoke {
            storage_root,
            bytes,
        } => {
            let (storage, _) = storage_command_context(&storage_root).await?;
            let provider = storage
                .providers_for_tier(StorageTier::Cold)
                .into_iter()
                .next()
                .context("no COLD provider is configured")?;
            if let Err(error) = run_storage_smoke(provider, bytes, false, false, "COLD").await {
                println!("diagnostic={}", mega_diagnostic(&error));
                return Err(error);
            }
        }
        StorageCommands::TelegramSmoke {
            storage_root,
            bytes,
            concurrency,
        } => {
            let (storage, _) = storage_command_context(&storage_root).await?;
            let provider = storage
                .providers_for_tier(StorageTier::Cold)
                .into_iter()
                .find(|provider| provider.provider_type().eq_ignore_ascii_case("telegram"))
                .context("no Telegram COLD provider is configured")?;
            run_telegram_pack_smoke(provider, bytes, &concurrency).await?;
        }
        StorageCommands::Health { storage_root } => {
            let (storage, database) = storage_command_context(&storage_root).await?;
            println!("storage_health=");
            println!("{}", serde_json::to_string_pretty(&storage.health().await)?);
            if let Some(database) = database {
                for record in database.list_storage_accounts(None).await? {
                    println!(
                        "account={} provider={} status={} capacity={} used={} reserved={} available={}",
                        record.snapshot.account_id,
                        record.snapshot.provider_id,
                        record.snapshot.status.as_str(),
                        record.snapshot.capacity_bytes,
                        record.snapshot.used_bytes,
                        record.snapshot.reserved_bytes,
                        record.snapshot.usable_free_bytes(),
                    );
                }
            }
        }
        StorageCommands::Probe {
            provider,
            live,
            storage_root,
        } => {
            let (storage, _) = storage_command_context(&storage_root).await?;
            let provider_id = if provider == "hot" {
                storage
                    .providers_for_tier(StorageTier::Hot)
                    .into_iter()
                    .next()
                    .map(|provider| provider.provider_id().to_owned())
                    .context("no HOT provider is configured")?
            } else if provider == "cold" {
                storage
                    .providers_for_tier(StorageTier::Cold)
                    .into_iter()
                    .next()
                    .map(|provider| provider.provider_id().to_owned())
                    .context("no COLD provider is configured")?
            } else {
                provider
            };
            let report = storage.probe_provider(&provider_id, live).await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            if live && report.steps.iter().any(|step| !step.passed) {
                anyhow::bail!("provider probe failed for {provider_id}");
            }
        }
        StorageCommands::Readiness { storage_root } => {
            let policy = StoragePolicy::from_env().map_err(|error| anyhow::anyhow!(error))?;
            let (storage, database) = storage_command_context(&storage_root).await?;
            let health = storage.health().await;
            let class_coverage = |class: StorageClass| {
                let healthy = health
                    .iter()
                    .filter(|provider| provider.storage_class == class && provider.healthy)
                    .collect::<Vec<_>>();
                let domains = healthy
                    .iter()
                    .map(|provider| provider.failure_domain.as_str())
                    .collect::<HashSet<_>>()
                    .len() as u32;
                (healthy.len() as u32, domains)
            };
            let (hot_healthy, hot_domains) = class_coverage(StorageClass::Hot);
            let (cold_healthy, cold_domains) = class_coverage(StorageClass::Cold);
            let (archive_healthy, archive_domains) = class_coverage(StorageClass::Archive);
            if hot_healthy < policy.required_replicas(StorageTier::Hot)
                || hot_domains < policy.required_failure_domains(StorageClass::Hot)
            {
                anyhow::bail!(
                    "staging readiness failed: hot coverage is {hot_healthy} replica(s)/{hot_domains} failure domain(s), required {}/{}",
                    policy.required_replicas(StorageTier::Hot),
                    policy.required_failure_domains(StorageClass::Hot),
                );
            }
            if cold_healthy < policy.required_replicas(StorageTier::Cold)
                || cold_domains < policy.required_failure_domains(StorageClass::Cold)
            {
                anyhow::bail!(
                    "staging readiness failed: cold coverage is {cold_healthy} replica(s)/{cold_domains} failure domain(s), required {}/{}",
                    policy.required_replicas(StorageTier::Cold),
                    policy.required_failure_domains(StorageClass::Cold),
                );
            }
            if archive_healthy < policy.required_replicas(StorageClass::Archive)
                || archive_domains < policy.required_failure_domains(StorageClass::Archive)
            {
                anyhow::bail!(
                    "staging readiness failed: archive coverage is {archive_healthy} replica(s)/{archive_domains} failure domain(s), required {}/{}",
                    policy.required_replicas(StorageClass::Archive),
                    policy.required_failure_domains(StorageClass::Archive),
                );
            }
            let cold_capacity_is_ledger_managed = storage
                .restore_sources(StorageClass::Cold)
                .iter()
                .any(|provider| provider.requires_capacity_account());
            if policy.required_replicas(StorageTier::Cold) > 0 && cold_capacity_is_ledger_managed {
                let database = database.context(
                    "staging readiness requires DATABASE_URL when cold backups are required",
                )?;
                let accounts = database.list_storage_accounts(None).await?;
                if accounts
                    .iter()
                    .filter(|record| record.snapshot.tier == StorageTier::Cold)
                    .all(|record| {
                        !matches!(
                            record.snapshot.status,
                            StorageAccountStatus::Active | StorageAccountStatus::NearFull
                        ) || record.snapshot.usable_free_bytes() == 0
                    })
                {
                    anyhow::bail!(
                        "staging readiness failed: every cold account is full or inside its safety margin"
                    );
                }
            }
            println!(
                "readiness=READY hot_healthy={} hot_failure_domains={} cold_healthy={} cold_failure_domains={} archive_healthy={} archive_failure_domains={} required_hot={} required_hot_failure_domains={} required_cold={} required_cold_failure_domains={} required_archive={} required_archive_failure_domains={}",
                hot_healthy,
                hot_domains,
                cold_healthy,
                cold_domains,
                archive_healthy,
                archive_domains,
                policy.required_replicas(StorageTier::Hot),
                policy.required_failure_domains(StorageClass::Hot),
                policy.required_replicas(StorageTier::Cold),
                policy.required_failure_domains(StorageClass::Cold),
                policy.required_replicas(StorageClass::Archive),
                policy.required_failure_domains(StorageClass::Archive),
            );
        }
        StorageCommands::Accounts { command } => handle_storage_account_command(command).await?,
        StorageCommands::Restore { encoded_hash } => {
            let database = command_database().await?;
            let storage_root = env::var_os("LAUNCHER_STORAGE_ROOT")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("storage"));
            let (storage, _) = storage_command_context(&storage_root).await?;
            let target_provider =
                env::var("LAUNCHER_RESTORE_TARGET_PROVIDER").unwrap_or_else(|_| "hot".to_owned());
            let job = database
                .enqueue_restore_job(&encoded_hash, &target_provider)
                .await?;
            process_restore_job(&database, &storage, &job).await?;
            println!("restore={} status=DONE", encoded_hash);
        }
        StorageCommands::RestorePending { limit } => {
            let database = command_database().await?;
            let jobs = database
                .list_restore_jobs(Some(&["QUEUED", "RUNNING", "RETRY"]), limit)
                .await?;
            for job in jobs {
                println!(
                    "restore_job={} hash={} target={} state={} attempts={}/{} error={}",
                    job.id,
                    job.encoded_hash,
                    job.target_provider,
                    job.state,
                    job.attempts,
                    job.max_attempts,
                    job.last_error.unwrap_or_default()
                );
            }
        }
        StorageCommands::Worker {
            worker_id,
            poll_seconds,
            storage_root,
        } => {
            let database = command_database().await?;
            let (storage, _) = storage_command_context(&storage_root).await?;
            let poll = Duration::from_secs(poll_seconds.clamp(1, 300));
            let mut cold_stream_server = env::var("LAUNCHER_COLD_STREAM_TOKEN")
                .ok()
                .filter(|token| !token.is_empty())
                .map(|token| {
                    let bind = env::var("LAUNCHER_COLD_STREAM_BIND").unwrap_or_else(|_| {
                        env::var("PORT")
                            .map(|port| format!("0.0.0.0:{port}"))
                            .unwrap_or_else(|_| "0.0.0.0:8081".to_owned())
                    });
                    tokio::spawn(run_cold_stream_server(
                        bind,
                        ColdStreamState {
                            storage: storage.clone(),
                            token: Arc::new(token),
                        },
                    ))
                });
            println!(
                "restore_worker=STARTED worker_id={worker_id} poll_seconds={} cold_stream={}",
                poll.as_secs(),
                cold_stream_server.is_some()
            );
            loop {
                database.recover_expired_restore_jobs().await?;
                database.recover_expired_pack_restore_jobs().await?;
                renew_due_hot_packs(&database, &storage, &worker_id, &storage_root).await?;
                if let Some(job) = database.claim_pack_restore_job(&worker_id, 600).await? {
                    if let Err(error) = process_pack_restore_job(&database, &storage, &job).await {
                        eprintln!("pack_restore_job={} status=RETRY error={error}", job.id);
                    } else {
                        println!(
                            "pack_restore_job={} status=DONE pack_hash={}",
                            job.id, job.pack_hash
                        );
                    }
                } else if let Some(job) = database.claim_restore_job(&worker_id, 600).await? {
                    if let Err(error) = process_restore_job(&database, &storage, &job).await {
                        eprintln!("restore_job={} status=RETRY error={error}", job.id);
                    } else {
                        println!(
                            "restore_job={} status=DONE hash={}",
                            job.id, job.encoded_hash
                        );
                    }
                } else {
                    if let Some(server) = cold_stream_server.as_mut() {
                        tokio::select! {
                            result = server => {
                                result??;
                                anyhow::bail!("cold stream server stopped unexpectedly");
                            }
                            _ = tokio::time::sleep(poll) => {}
                        }
                    } else {
                        tokio::time::sleep(poll).await;
                    }
                }
            }
        }
        StorageCommands::ColdRestoreSmoke {
            build_id,
            encoded_hash,
            target_provider,
            confirm,
            storage_root,
        } => {
            run_cold_restore_smoke(
                &build_id,
                &encoded_hash,
                &target_provider,
                confirm,
                &storage_root,
            )
            .await?;
        }
        StorageCommands::ColdPackRestoreSmoke {
            build_id,
            pack_hash,
            target_provider,
            confirm,
            metadata_only,
            storage_root,
        } => {
            run_cold_pack_restore_smoke(
                &build_id,
                &pack_hash,
                &target_provider,
                confirm,
                metadata_only,
                &storage_root,
            )
            .await?;
        }
        StorageCommands::Gc { dry_run, limit } => {
            let database = command_database().await?;
            let objects = database.list_unreachable_storage_objects(limit).await?;
            for object in &objects {
                println!(
                    "gc_candidate hash={} provider={} tier={} key={}",
                    object.encoded_hash, object.provider, object.tier, object.object_key
                );
            }
            if !dry_run {
                let storage_root = env::var_os("LAUNCHER_STORAGE_ROOT")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("storage"));
                let (storage, _) = storage_command_context(&storage_root).await?;
                for object in objects {
                    let provider = storage.provider(&object.provider).with_context(|| {
                        format!("provider {} is not configured", object.provider)
                    })?;
                    provider.delete_encoded(&object.encoded_hash).await?;
                    database
                        .delete_storage_object(&object.encoded_hash, &object.provider)
                        .await?;
                }
            }
            println!(
                "gc={} mode={}",
                if dry_run { "DRY_RUN" } else { "APPLIED" },
                if dry_run { "dry-run" } else { "apply" }
            );
        }
    }
    Ok(())
}

fn select_provider(
    storage: &StorageRegistry,
    selector: &str,
    tier: StorageTier,
) -> Result<Arc<dyn StorageProvider>> {
    if !selector.eq_ignore_ascii_case("hot") && !selector.eq_ignore_ascii_case("cold") {
        return storage
            .provider(selector)
            .or_else(|| storage.providers_for_pool(selector).into_iter().next())
            .with_context(|| format!("provider {selector:?} is not configured"));
    }
    storage
        .providers_for_tier(tier)
        .into_iter()
        .next()
        .with_context(|| format!("no {} provider is configured", tier.as_str()))
}

async fn run_storage_smoke(
    provider: Arc<dyn StorageProvider>,
    requested_bytes: usize,
    fetch_download_url: bool,
    upload_only: bool,
    tier_label: &str,
) -> Result<()> {
    let byte_count = requested_bytes.clamp(1, 4 * 1024 * 1024);
    let mut bytes = vec![0_u8; byte_count];
    OsRng.fill_bytes(&mut bytes);
    let encoded_hash = blake3::hash(&bytes).to_hex().to_string();
    let provider_id = provider.provider_id().to_owned();
    let delete_supported = provider.capabilities().delete;
    let result = async {
        provider.health_check().await?;
        println!("check={tier_label}_health status=PASS provider={provider_id}");
        provider.put_encoded(&encoded_hash, &bytes).await?;
        println!(
            "check={tier_label}_put status=PASS bytes={} hash={encoded_hash}",
            bytes.len()
        );
        if upload_only {
            println!("check={tier_label}_head status=SKIP reason=upload_only_provider_capability");
            println!("check={tier_label}_get status=SKIP reason=upload_only_provider_capability");
        } else {
            let head_size = provider
                .head_encoded(&encoded_hash)
                .await?
                .context("smoke object was not found after upload")?;
            if head_size != bytes.len() as u64 {
                anyhow::bail!(
                    "smoke HEAD size mismatch: expected {}, got {head_size}",
                    bytes.len()
                );
            }
            println!("check={tier_label}_head status=PASS size={head_size}");
            let downloaded = provider.read_encoded(&encoded_hash).await?;
            if downloaded != bytes {
                anyhow::bail!("smoke GET bytes did not match uploaded bytes");
            }
            println!("check={tier_label}_get status=PASS blake3={encoded_hash}");
        }
        if fetch_download_url {
            let location = provider.download_location(&encoded_hash).await?;
            let response = reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()?
                .get(&location.url)
                .send()
                .await?
                .error_for_status()?;
            let direct_bytes = response.bytes().await?;
            if direct_bytes.as_ref() != bytes.as_slice() {
                anyhow::bail!("download URL returned bytes different from the smoke object");
            }
            println!("check={tier_label}_direct_download status=PASS");
            println!(
                "check={tier_label}_download_url status=PASS expires_at={}",
                location
                    .expires_at
                    .map(|value| value.to_rfc3339())
                    .unwrap_or_else(|| "none".to_owned())
            );
        } else {
            println!(
                "check={tier_label}_download_url status=SKIP reason=server_side_only_or_disabled"
            );
        }
        Ok::<(), anyhow::Error>(())
    }
    .await;
    let cleanup = if delete_supported {
        Some(provider.delete_encoded(&encoded_hash).await)
    } else {
        None
    };
    match result {
        Ok(()) => {
            if let Some(cleanup) = cleanup {
                cleanup?;
                if provider.head_encoded(&encoded_hash).await?.is_some() {
                    anyhow::bail!("smoke DELETE did not remove the temporary object");
                }
                println!("check={tier_label}_delete status=PASS");
            } else {
                println!("check={tier_label}_delete status=SKIP reason=provider_capability_false");
            }
            println!(
                "storage_smoke=PASS provider={provider_id} bytes={}",
                bytes.len()
            );
            Ok(())
        }
        Err(error) => {
            if let Some(cleanup) = cleanup {
                let _ = cleanup;
            }
            Err(error)
        }
    }
}

async fn run_telegram_pack_smoke(
    provider: Arc<dyn StorageProvider>,
    requested_bytes: usize,
    concurrency: &[usize],
) -> Result<()> {
    if !provider.provider_type().eq_ignore_ascii_case("telegram") {
        anyhow::bail!(
            "telegram smoke selected provider {}",
            provider.provider_type()
        );
    }
    let requested_bytes = requested_bytes.clamp(1, 512 * 1024 * 1024);
    let raw_size = requested_bytes.saturating_sub(2 * 1024 * 1024).max(1);
    let mut raw = vec![0_u8; raw_size];
    OsRng.fill_bytes(&mut raw);
    let encoded = zstd::bulk::compress(&raw, 3)?;
    let encoded_hash = blake3::hash(&encoded).to_hex().to_string();
    let raw_hash = blake3::hash(&raw).to_hex().to_string();
    let pack_limit = requested_bytes.max(8 * 1024 * 1024) as u64;
    let pack = launcher_packs::build_packs(
        [launcher_packs::PackInput::new(
            encoded_hash,
            raw_hash,
            raw.len() as u64,
            encoded,
        )],
        launcher_packs::PackConfig {
            target_bytes: pack_limit,
            min_bytes: 1,
            max_bytes: pack_limit,
        },
    )?
    .into_iter()
    .next()
    .context("pack smoke did not produce a physical pack")?;
    let pack_hash = pack.pack_hash;
    let pack_bytes = pack.bytes;
    provider.health_check().await?;
    println!("check=TELEGRAM_network status=PASS");
    provider.put_pack(&pack_hash, &pack_bytes).await?;
    println!(
        "check=TELEGRAM_upload status=PASS bytes={} pack_hash={pack_hash}",
        pack_bytes.len()
    );
    let restored = provider.read_pack(&pack_hash).await?;
    if restored != pack_bytes {
        anyhow::bail!("Telegram pack bytes did not match the uploaded smoke pack");
    }
    launcher_packs::PackReader::parse(&restored)?.verify_pack_hash(&pack_hash)?;
    println!("check=TELEGRAM_download status=PASS");
    println!("check=TELEGRAM_integrity status=PASS blake3={pack_hash}");

    let mut levels = concurrency.to_vec();
    if levels.is_empty() {
        levels.push(1);
    }
    levels.sort_unstable();
    levels.dedup();
    for level in levels {
        if level == 0 || level > 16 {
            anyhow::bail!("Telegram smoke concurrency must be between 1 and 16, got {level}");
        }
        let started = std::time::Instant::now();
        let mut tasks = Vec::with_capacity(level);
        for _ in 0..level {
            let provider = provider.clone();
            let expected_hash = pack_hash.clone();
            tasks.push(tokio::spawn(async move {
                stream_pack_hash(provider, &expected_hash).await
            }));
        }
        let mut total_bytes = 0_u64;
        for task in tasks {
            let (bytes, hash) = task
                .await
                .context("Telegram restore benchmark task panicked")??;
            if hash != pack_hash {
                anyhow::bail!(
                    "Telegram restore benchmark hash mismatch: expected {pack_hash}, got {hash}"
                );
            }
            total_bytes = total_bytes
                .checked_add(bytes)
                .context("Telegram restore benchmark byte count overflow")?;
        }
        let elapsed = started.elapsed();
        let seconds = elapsed.as_secs_f64().max(f64::EPSILON);
        let throughput_mbps = total_bytes as f64 / seconds / 1024.0 / 1024.0;
        println!(
            "telegram_restore_benchmark concurrency={level} requests={level} bytes={total_bytes} elapsed_ms={} throughput_mib_s={throughput_mbps:.2}",
            elapsed.as_millis()
        );
    }

    provider.delete_pack(&pack_hash).await?;
    if provider.read_pack(&pack_hash).await.is_ok() {
        anyhow::bail!("Telegram smoke delete left the temporary pack readable");
    }
    println!("check=TELEGRAM_delete status=PASS");
    println!("telegram_smoke=PASS pack_bytes={}", pack_bytes.len());
    Ok(())
}

async fn stream_pack_hash(
    provider: Arc<dyn StorageProvider>,
    pack_hash: &str,
) -> Result<(u64, String)> {
    let mut stream = provider.read_pack_stream(pack_hash).await?;
    let mut hasher = blake3::Hasher::new();
    let mut total = 0_u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        total = total
            .checked_add(chunk.len() as u64)
            .context("Telegram restore stream byte count overflow")?;
        hasher.update(&chunk);
    }
    Ok((total, hasher.finalize().to_hex().to_string()))
}

fn mega_diagnostic(error: &anyhow::Error) -> &'static str {
    let message = error.to_string().to_ascii_lowercase();
    if message.contains("authentication")
        || message.contains("not logged")
        || message.contains("login")
        || message.contains("session")
    {
        "MEGA_AUTH_FAILED"
    } else if message.contains("network")
        || message.contains("dns")
        || message.contains("resolve")
        || message.contains("timed out")
        || message.contains("timeout")
        || message.contains("connection")
        || message.contains("unreachable")
        || message.contains("refused")
    {
        "MEGA_NETWORK_UNAVAILABLE"
    } else if message.contains("no such file")
        || message.contains("not found")
        || message.contains("mega-whoami")
        || message.contains("megacmd")
    {
        "MEGA_RUNTIME_MISSING"
    } else {
        "MEGA_PROVIDER_UNAVAILABLE"
    }
}

async fn run_cold_restore_smoke(
    build_id: &str,
    encoded_hash: &str,
    target_provider: &str,
    confirm: bool,
    storage_root: &Path,
) -> Result<()> {
    if !confirm {
        anyhow::bail!(
            "cold restore smoke deletes one HOT object; pass --confirm after selecting a staging-only build"
        );
    }
    if !build_id.starts_with("staging-") {
        anyhow::bail!("cold restore smoke only accepts build IDs beginning with staging-");
    }
    let database = command_database().await?;
    let manifest = database
        .get_manifest(build_id)
        .await?
        .with_context(|| format!("published build {build_id:?} was not found"))?;
    if !manifest
        .files
        .iter()
        .flat_map(|file| file.chunks.iter())
        .any(|chunk| chunk.encoded_hash == encoded_hash)
    {
        anyhow::bail!("encoded hash is not referenced by the selected staging build");
    }
    if database
        .count_published_build_references(encoded_hash)
        .await?
        != 1
    {
        anyhow::bail!(
            "encoded hash is shared by multiple published/ready builds; refusing destructive smoke"
        );
    }
    let (storage, _) = storage_command_context(storage_root).await?;
    let hot = select_provider(&storage, target_provider, StorageTier::Hot)?;
    let cold = storage
        .restore_sources(StorageClass::Cold)
        .into_iter()
        .next()
        .context("no COLD provider is configured")?;
    let hot_size = hot
        .head_encoded(encoded_hash)
        .await?
        .context("selected HOT object is already missing")?;
    let cold_size = cold
        .head_encoded(encoded_hash)
        .await?
        .context("selected COLD object is missing")?;
    println!(
        "cold_restore=SELECTED build={build_id} hash={encoded_hash} hot_provider={} hot_size={hot_size} cold_provider={} cold_size={cold_size}",
        hot.provider_id(),
        cold.provider_id()
    );
    hot.delete_encoded(encoded_hash).await?;
    database
        .delete_storage_object(encoded_hash, hot.provider_id())
        .await?;
    let job = database
        .enqueue_restore_job(encoded_hash, hot.provider_id())
        .await?;
    process_restore_job(&database, &storage, &job).await?;
    let restored_size = hot
        .head_encoded(encoded_hash)
        .await?
        .context("restore completed without a HOT object")?;
    let restored = hot.read_encoded(encoded_hash).await?;
    if restored_size != restored.len() as u64 {
        anyhow::bail!("restored HOT object size changed during verification");
    }
    println!(
        "cold_restore=PASS job={} restored_provider={} restored_size={} blake3={encoded_hash}",
        job.id,
        hot.provider_id(),
        restored_size
    );
    Ok(())
}

async fn run_cold_pack_restore_smoke(
    build_id: &str,
    pack_hash: &str,
    target_provider: &str,
    confirm: bool,
    metadata_only: bool,
    storage_root: &Path,
) -> Result<()> {
    if !confirm {
        anyhow::bail!(
            "cold pack restore smoke deletes one HOT physical pack; pass --confirm after selecting a staging-only build"
        );
    }
    if !build_id.starts_with("staging-") {
        anyhow::bail!("cold pack restore smoke only accepts build IDs beginning with staging-");
    }
    if pack_hash.len() != 64 || !pack_hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        anyhow::bail!("pack hash must be a 64-character hexadecimal BLAKE3 digest");
    }

    let database = command_database().await?;
    if !database
        .cold_pack_available_for_build(build_id, pack_hash)
        .await?
    {
        anyhow::bail!(
            "physical pack {pack_hash} is not a verified COLD location for build {build_id}"
        );
    }
    let (storage, _) = storage_command_context(storage_root).await?;
    let hot = select_provider(&storage, target_provider, StorageTier::Hot)?;
    let cold = storage
        .restore_sources(StorageClass::Cold)
        .into_iter()
        .next()
        .context("no COLD provider is configured")?;

    let original = hot
        .read_pack(pack_hash)
        .await
        .context("selected HOT physical pack is already missing or unreadable")?;
    launcher_packs::PackReader::parse(&original)?.verify_pack_hash(pack_hash)?;
    let cold_copy = cold
        .read_pack(pack_hash)
        .await
        .context("selected COLD physical pack is missing or unreadable")?;
    launcher_packs::PackReader::parse(&cold_copy)?.verify_pack_hash(pack_hash)?;
    println!(
        "cold_pack_restore=SELECTED build={build_id} pack_hash={pack_hash} hot_provider={} cold_provider={} bytes={}",
        hot.provider_id(),
        cold.provider_id(),
        original.len()
    );

    if metadata_only {
        hot.forget_pack_reference(pack_hash).await?;
        println!(
            "cold_pack_restore=HOT_REFERENCE_EVICTED provider={} remote_delete=NOT_RUN reason=provider_delete_not_proven",
            hot.provider_id()
        );
    } else {
        hot.delete_pack(pack_hash).await?;
    }
    database
        .delete_pack_locations_for_provider(pack_hash, hot.provider_id())
        .await?;
    if hot.read_pack(pack_hash).await.is_ok() {
        anyhow::bail!("HOT physical pack remained readable after deliberate deletion");
    }

    let job = database
        .enqueue_pack_restore_job(pack_hash, hot.provider_id())
        .await?;
    process_pack_restore_job(&database, &storage, &job).await?;
    let restored = hot
        .read_pack(pack_hash)
        .await
        .context("pack restore completed without a HOT physical pack")?;
    launcher_packs::PackReader::parse(&restored)?.verify_pack_hash(pack_hash)?;
    println!(
        "cold_pack_restore=PASS job={} source_provider={} restored_provider={} restored_bytes={} blake3={pack_hash}",
        job.id,
        cold.provider_id(),
        hot.provider_id(),
        restored.len()
    );
    Ok(())
}

async fn connect_database() -> Result<Database> {
    let url = env::var("DATABASE_URL")
        .context("DATABASE_URL is required for this storage admin command")?;
    Ok(Database::connect(&url).await?)
}

async fn command_database() -> Result<Database> {
    let database = connect_database().await?;
    database.migrate().await?;
    let storage_root = env::var_os("LAUNCHER_STORAGE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("storage"));
    let base_url =
        env::var("LAUNCHER_PUBLIC_BASE_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_owned());
    let reservation_store: Arc<dyn CapacityReservationStore> = Arc::new(database.clone());
    let (storage, _) =
        storage_from_env_with_reservation_store(&storage_root, base_url, reservation_store).await?;
    database.ensure_storage_pools(storage.pools()).await?;
    Ok(database)
}

fn provisioning_manager(database: &Database) -> ProvisioningManager {
    let secret_root = env::var("PROVISIONING_SECRET_STORE_DIR")
        .unwrap_or_else(|_| "provisioning-secrets".to_owned());
    let secret_store = Arc::new(FileSecretStore::new(secret_root));
    let mut registry = ProvisionerRegistry::default();
    registry.register("mega", manual_mega_provisioner());
    if env_bool("PROVISIONING_ENABLE_FAKE", false) {
        registry.register_fake("fake", secret_store);
    }
    let alias_domain =
        env::var("PROVISIONING_EMAIL_DOMAIN").unwrap_or_else(|_| "vaultnode.pp.ua".to_owned());
    let alias_ttl = env::var("PROVISIONING_MAIL_ALIAS_TTL_SECONDS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(3600)
        .clamp(60, 86_400);
    ProvisioningManager::new(Arc::new(database.clone()), registry)
        .with_email_config(alias_domain, chrono::Duration::seconds(alias_ttl))
        .with_validator(
            "mega",
            Arc::new(MegaCandidateValidator {
                database: database.clone(),
            }),
            Arc::new(MegaCandidateEnroller {
                database: database.clone(),
            }),
        )
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

async fn handle_provisioning_command(command: ProvisioningCommands) -> Result<()> {
    let database = command_database().await?;
    match command {
        ProvisioningCommands::List { status, limit } => {
            let status = status
                .as_deref()
                .map(str::parse::<ProvisioningStatus>)
                .transpose()
                .context("status must be one of the provisioning state names")?;
            let jobs = database
                .list_jobs(status, limit)
                .await
                .map_err(provisioning_anyhow)?;
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &jobs.iter().map(provisioning_job_view).collect::<Vec<_>>()
                )?
            );
        }
        ProvisioningCommands::Inspect { job_id } => {
            let id = parse_provisioning_job_id(&job_id)?;
            let job = database
                .get_job(id)
                .await
                .map_err(provisioning_anyhow)?
                .with_context(|| format!("provisioning job {job_id} was not found"))?;
            println!(
                "{}",
                serde_json::to_string_pretty(&provisioning_job_view(&job))?
            );
        }
        ProvisioningCommands::Retry { job_id } => {
            let manager = provisioning_manager(&database);
            let job = manager
                .retry_job(parse_provisioning_job_id(&job_id)?)
                .await
                .map_err(provisioning_anyhow)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&provisioning_job_view(&job))?
            );
        }
        ProvisioningCommands::Cancel { job_id, reason } => {
            let manager = provisioning_manager(&database);
            let job = manager
                .cancel_job(parse_provisioning_job_id(&job_id)?, reason)
                .await
                .map_err(provisioning_anyhow)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&provisioning_job_view(&job))?
            );
        }
        ProvisioningCommands::CompleteManual {
            job_id,
            candidate_reference,
            credential_reference,
            expected_capacity_bytes,
            provider_type,
        } => {
            let credential_reference = SecretRef::parse(credential_reference)
                .context("credential-reference must be a SecretRef using secret://")?;
            let candidate = CapacityCandidate {
                provider_type,
                external_account_id: candidate_reference,
                credential_reference,
                expected_capacity_bytes,
                metadata: Default::default(),
            };
            let manager = provisioning_manager(&database);
            let job = manager
                .complete_manual(parse_provisioning_job_id(&job_id)?, candidate)
                .await
                .map_err(provisioning_anyhow)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&provisioning_job_view(&job))?
            );
        }
        ProvisioningCommands::Readiness => {
            let domain = env::var("PROVISIONING_EMAIL_DOMAIN")
                .unwrap_or_else(|_| "vaultnode.pp.ua".to_owned());
            let hmac_configured = env::var("PROVISIONING_EMAIL_INGEST_HMAC_SECRET")
                .ok()
                .is_some_and(|secret| !secret.is_empty());
            let manager = provisioning_manager(&database);
            let active_jobs = database
                .list_jobs(None, 500)
                .await
                .map_err(provisioning_anyhow)?
                .into_iter()
                .filter(|job| job.status.is_active())
                .count();
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "database": "READY",
                    "email_domain": domain,
                    "email_hmac_configured": hmac_configured,
                    "secret_store_configured": env::var("PROVISIONING_SECRET_STORE_DIR").is_ok(),
                    "active_jobs": active_jobs,
                    "capabilities": manager.capabilities(),
                    "manual_mode_is_valid": true,
                }))?
            );
        }
        ProvisioningCommands::TestEmailAddress { address } => {
            let domain = env::var("PROVISIONING_EMAIL_DOMAIN")
                .unwrap_or_else(|_| "vaultnode.pp.ua".to_owned());
            let Some(address) = address else {
                anyhow::ensure!(
                    env_bool("PROVISIONING_ENABLE_FAKE", false),
                    "generating a test alias requires PROVISIONING_ENABLE_FAKE=true; this creates no real provider capacity"
                );
                let pool_id = env::var("PROVISIONING_EMAIL_TEST_POOL_ID")
                    .unwrap_or_else(|_| "railway-hot".to_owned());
                let ttl_seconds = env::var("PROVISIONING_EMAIL_TEST_TTL_SECONDS")
                    .ok()
                    .and_then(|value| value.parse::<i64>().ok())
                    .unwrap_or(900)
                    .clamp(60, 3600);
                let job = provisioning_manager(&database)
                    .ensure_capacity(ProvisionRequest {
                        provider_type: "fake".to_owned(),
                        pool_id,
                        requested_capacity_bytes: 1024,
                        expires_at: Utc::now() + chrono::Duration::seconds(ttl_seconds),
                        idempotency_key: format!("email-smoke-{}", uuid::Uuid::new_v4()),
                    })
                    .await
                    .map_err(provisioning_anyhow)?;
                let address = job
                    .inbound_email_address
                    .clone()
                    .context("test provisioning job did not receive an email alias")?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "test_job_created": true,
                        "job_id": job.id,
                        "address": address,
                        "status": job.status,
                        "expires_at": job.inbound_email_expires_at,
                        "active_job": true,
                        "provider_type": "fake",
                        "real_capacity_allocated": false,
                    }))?
                );
                return Ok(());
            };
            let normalized = address.trim().to_ascii_lowercase();
            let syntactically_valid = normalized.split_once('@').is_some_and(|(local, host)| {
                !local.is_empty() && host == domain.trim().to_ascii_lowercase()
            });
            let active = if syntactically_valid {
                database
                    .find_active_job_by_email(&normalized)
                    .await
                    .map_err(provisioning_anyhow)?
                    .is_some()
            } else {
                false
            };
            println!(
                "{}",
                serde_json::json!({
                    "address_valid": syntactically_valid,
                    "active_job": active,
                })
            );
        }
        ProvisioningCommands::EnsureCapacity {
            provider_type,
            pool_id,
            requested_capacity_bytes,
            idempotency_key,
            expires_seconds,
        } => {
            let manager = provisioning_manager(&database);
            let job = manager
                .ensure_capacity(ProvisionRequest {
                    provider_type,
                    pool_id,
                    requested_capacity_bytes,
                    expires_at: Utc::now() + chrono::Duration::seconds(expires_seconds.max(60)),
                    idempotency_key,
                })
                .await
                .map_err(provisioning_anyhow)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&provisioning_job_view(&job))?
            );
        }
        ProvisioningCommands::Worker { poll_seconds } => {
            let manager = provisioning_manager(&database);
            loop {
                let jobs = database
                    .list_jobs(None, 500)
                    .await
                    .map_err(provisioning_anyhow)?;
                let now = Utc::now();
                for job in jobs {
                    if job.status.is_active() && job.expires_at <= now {
                        let _ = manager.expire_job(job.id).await;
                    } else if job.status == ProvisioningStatus::FailedRetryable
                        && job.retry_after.is_none_or(|retry_after| retry_after <= now)
                    {
                        let _ = manager.retry_job(job.id).await;
                    }
                }
                tokio::time::sleep(Duration::from_secs(poll_seconds.max(1))).await;
            }
        }
    }
    Ok(())
}

fn parse_provisioning_job_id(value: &str) -> Result<uuid::Uuid> {
    uuid::Uuid::parse_str(value).with_context(|| format!("invalid provisioning job UUID {value}"))
}

fn provisioning_anyhow(error: ProvisioningError) -> anyhow::Error {
    anyhow::anyhow!(error.to_string())
}

fn provisioning_job_view(job: &launcher_provisioning::ProvisioningJob) -> serde_json::Value {
    serde_json::json!({
        "id": job.id,
        "provider_type": job.provider_type,
        "pool_id": job.pool_id,
        "requested_capacity_bytes": job.requested_capacity_bytes,
        "status": job.status,
        "attempt_count": job.attempt_count,
        "created_at": job.created_at,
        "updated_at": job.updated_at,
        "started_at": job.started_at,
        "completed_at": job.completed_at,
        "last_error_code": job.last_error_code,
        "last_error_summary": job.last_error_summary,
        "inbound_email_address": job.inbound_email_address,
        "inbound_email_expires_at": job.inbound_email_expires_at,
        "candidate_configured": job.candidate_reference.is_some(),
        "credential_configured": job.credential_reference.is_some(),
        "operator_action": job.operator_action,
        "retry_after": job.retry_after,
        "expires_at": job.expires_at,
        "idempotency_key": job.idempotency_key,
    })
}

struct MegaCandidateValidator {
    database: Database,
}

#[async_trait]
impl CapacityCandidateValidator for MegaCandidateValidator {
    async fn validate(
        &self,
        candidate: &CapacityCandidate,
        requested_capacity_bytes: u64,
    ) -> Result<ValidatedCapacity, ProvisioningError> {
        let account = self
            .database
            .list_storage_accounts(None)
            .await
            .map_err(|_| {
                ProvisioningError::Provider("could not read MEGA account ledger".to_owned())
            })?
            .into_iter()
            .find(|record| record.snapshot.account_id == candidate.external_account_id)
            .ok_or_else(|| {
                ProvisioningError::Provider("MEGA account is not enrolled in the ledger".to_owned())
            })?;
        let config: MegaAccountConfig = serde_json::from_value(account.configuration_json)
            .map_err(|_| {
                ProvisioningError::Provider("MEGA account configuration is invalid".to_owned())
            })?;
        let mega = MegaCliAccount::new(config).map_err(|_| {
            ProvisioningError::Provider("MEGA account configuration is invalid".to_owned())
        })?;
        mega.health().await.map_err(|_| {
            ProvisioningError::Provider("MEGA authentication validation failed".to_owned())
        })?;
        let capacity = mega
            .capacity()
            .await
            .map_err(|_| ProvisioningError::Provider("MEGA capacity query failed".to_owned()))?;
        let available = capacity.capacity_bytes.saturating_sub(capacity.used_bytes);
        if available < requested_capacity_bytes {
            return Err(ProvisioningError::Provider(
                "MEGA account does not have the requested capacity".to_owned(),
            ));
        }
        let mut payload = vec![0_u8; 1024];
        OsRng.fill_bytes(&mut payload);
        let digest = blake3::hash(&payload);
        let id = uuid::Uuid::new_v4();
        let temp_dir = env::var("PROVISIONING_TEMP_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| env::temp_dir());
        tokio::fs::create_dir_all(&temp_dir).await.map_err(|_| {
            ProvisioningError::Provider("could not prepare bounded validation space".to_owned())
        })?;
        let upload_path = temp_dir.join(format!("launcher-provisioning-{id}.upload"));
        let download_path = temp_dir.join(format!("launcher-provisioning-{id}.download"));
        tokio::fs::write(&upload_path, &payload)
            .await
            .map_err(|_| {
                ProvisioningError::Provider("could not write validation object".to_owned())
            })?;
        let remote_path = format!(
            "{}/.launcher-provisioning/{id}",
            mega.remote_root().trim_end_matches('/')
        );
        let transfer = async {
            mega.upload_file(&upload_path, &remote_path)
                .await
                .map_err(|_| {
                    ProvisioningError::Provider("MEGA validation upload failed".to_owned())
                })?;
            mega.download_file(&remote_path, &download_path)
                .await
                .map_err(|_| {
                    ProvisioningError::Provider("MEGA validation download failed".to_owned())
                })?;
            let downloaded = tokio::fs::read(&download_path).await.map_err(|_| {
                ProvisioningError::Provider("could not read validation download".to_owned())
            })?;
            if blake3::hash(&downloaded) != digest {
                return Err(ProvisioningError::Provider(
                    "MEGA validation BLAKE3 integrity check failed".to_owned(),
                ));
            }
            mega.delete_object(&remote_path).await.map_err(|_| {
                ProvisioningError::Provider("MEGA validation delete failed".to_owned())
            })?;
            Ok::<(), ProvisioningError>(())
        }
        .await;
        let _ = tokio::fs::remove_file(&upload_path).await;
        let _ = tokio::fs::remove_file(&download_path).await;
        if transfer.is_err() {
            let _ = mega.delete_object(&remote_path).await;
        }
        transfer?;
        Ok(ValidatedCapacity {
            candidate: candidate.clone(),
            observed_capacity_bytes: capacity.capacity_bytes,
        })
    }
}

struct MegaCandidateEnroller {
    database: Database,
}

#[async_trait]
impl CapacityCandidateEnroller for MegaCandidateEnroller {
    async fn enroll(
        &self,
        _pool_id: &str,
        validated: &ValidatedCapacity,
    ) -> Result<(), ProvisioningError> {
        self.database
            .set_storage_account_status(
                &validated.candidate.external_account_id,
                StorageAccountStatus::Active,
                None,
            )
            .await
            .map_err(|_| {
                ProvisioningError::Provider("could not mark MEGA account active".to_owned())
            })
    }
}

async fn handle_storage_account_command(command: StorageAccountCommands) -> Result<()> {
    match command {
        StorageAccountCommands::Add {
            account_id,
            credential_reference,
            home_dir,
            remote_root,
            command_dir,
            mut capacity_bytes,
            safety_margin_bytes,
            provider_id,
        } => {
            let config = MegaAccountConfig {
                account_id,
                credential_reference,
                command_dir,
                home_dir,
                remote_root,
                capacity_bytes,
                safety_margin_bytes,
                timeout_seconds: 120,
                max_output_bytes: 64 * 1024,
            };
            let account = MegaCliAccount::new(config.clone())?;
            let health = account.health().await;
            let capacity = account.capacity().await;
            if capacity_bytes == 0
                && let Ok(snapshot) = &capacity
            {
                capacity_bytes = snapshot.capacity_bytes;
            }
            let mut persisted_config = config;
            persisted_config.capacity_bytes = capacity_bytes;
            upsert_mega_config(&provider_id, persisted_config.clone())?;
            let database = command_database().await?;
            database
                .upsert_storage_provider(
                    &provider_id,
                    "mega",
                    StorageTier::Cold,
                    serde_json::json!({"managed_by":"MEGAcmd"}),
                )
                .await?;
            let status = match (&health, &capacity) {
                (Ok(()), Ok(_)) => StorageAccountStatus::Active,
                (Err(launcher_storage::StorageError::Authentication(_)), _)
                | (_, Err(launcher_storage::StorageError::Authentication(_))) => {
                    StorageAccountStatus::AuthFailed
                }
                _ => StorageAccountStatus::Unavailable,
            };
            database
                .upsert_storage_account(&provider_id, &persisted_config, status)
                .await?;
            let health_error = health
                .as_ref()
                .err()
                .map(ToString::to_string)
                .or_else(|| capacity.as_ref().err().map(ToString::to_string));
            if matches!(
                &health,
                Err(launcher_storage::StorageError::NetworkUnavailable(_))
            ) || matches!(
                &capacity,
                Err(launcher_storage::StorageError::NetworkUnavailable(_))
            ) {
                println!("diagnostic=MEGA_NETWORK_UNAVAILABLE");
            }
            database
                .set_storage_account_status(
                    &persisted_config.account_id,
                    status,
                    health_error.as_deref(),
                )
                .await?;
            println!(
                "account={} provider={} status={} credential_configured={} capacity_bytes={}",
                persisted_config.account_id,
                provider_id,
                status.as_str(),
                !persisted_config.credential_reference.is_empty(),
                persisted_config.capacity_bytes
            );
            health.map_err(|error| anyhow::anyhow!(error))?;
            capacity.map_err(|error| anyhow::anyhow!(error))?;
        }
        StorageAccountCommands::List { provider_id } => {
            let database = command_database().await?;
            for record in database
                .list_storage_accounts(provider_id.as_deref())
                .await?
            {
                println!(
                    "account={} provider={} status={} credential_configured={} capacity={} used={} reserved={} available={}",
                    record.snapshot.account_id,
                    record.snapshot.provider_id,
                    record.snapshot.status.as_str(),
                    !record.credential_reference.is_empty(),
                    record.snapshot.capacity_bytes,
                    record.snapshot.used_bytes,
                    record.snapshot.reserved_bytes,
                    record.snapshot.usable_free_bytes(),
                );
            }
        }
        StorageAccountCommands::Inspect { account_id } => {
            let database = command_database().await?;
            let record = database
                .list_storage_accounts(None)
                .await?
                .into_iter()
                .find(|record| record.snapshot.account_id == account_id)
                .with_context(|| format!("unknown storage account {account_id}"))?;
            println!(
                "account={} provider={} tier={} status={} credential_configured={} capacity={} used={} reserved={} available={} last_capacity_check={:?}",
                record.snapshot.account_id,
                record.snapshot.provider_id,
                record.snapshot.tier,
                record.snapshot.status.as_str(),
                !record.credential_reference.is_empty(),
                record.snapshot.capacity_bytes,
                record.snapshot.used_bytes,
                record.snapshot.reserved_bytes,
                record.snapshot.usable_free_bytes(),
                record.snapshot.last_capacity_check,
            );
        }
        StorageAccountCommands::Reauth { account_id } => {
            let database = command_database().await?;
            let record = database
                .list_storage_accounts(None)
                .await?
                .into_iter()
                .find(|record| record.snapshot.account_id == account_id)
                .with_context(|| format!("unknown storage account {account_id}"))?;
            let config: MegaAccountConfig = serde_json::from_value(record.configuration_json)
                .context("stored MEGA account configuration is invalid")?;
            let account = MegaCliAccount::new(config)?;
            account.health().await?;
            let capacity = account.capacity().await?;
            let reservation_store: Arc<dyn CapacityReservationStore> = Arc::new(database.clone());
            reservation_store
                .refresh_account_capacity(&account_id, capacity)
                .await?;
            database
                .set_storage_account_status(&account_id, StorageAccountStatus::Active, None)
                .await?;
            println!("account={} status=ACTIVE reauth=SESSION_REUSED", account_id);
        }
        StorageAccountCommands::Disable { account_id } => {
            let database = command_database().await?;
            database
                .set_storage_account_status(&account_id, StorageAccountStatus::Disabled, None)
                .await?;
            println!("account={} status=DISABLED", account_id);
        }
    }
    Ok(())
}

fn upsert_mega_config(provider_id: &str, account: MegaAccountConfig) -> Result<()> {
    let path = env::var("LAUNCHER_MEGA_ACCOUNTS_FILE")
        .context("LAUNCHER_MEGA_ACCOUNTS_FILE is required to enroll a MEGA account")?;
    let path = PathBuf::from(path);
    let mut config = if path.exists() {
        MegaColdStorageConfig::from_file(&path)?
    } else {
        MegaColdStorageConfig {
            provider_id: provider_id.to_owned(),
            accounts: Vec::new(),
            tier: StorageTier::Cold,
            reservation_ttl_seconds: 3600,
            verify_existing: true,
        }
    };
    if config.provider_id != provider_id {
        anyhow::bail!(
            "MEGA config provider ID {} does not match requested {}",
            config.provider_id,
            provider_id
        );
    }
    if let Some(existing) = config
        .accounts
        .iter_mut()
        .find(|existing| existing.account_id == account.account_id)
    {
        *existing = account;
    } else {
        config.accounts.push(account);
    }
    config.validate().map_err(|error| anyhow::anyhow!(error))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_vec_pretty(&config)?)?;
    Ok(())
}

async fn process_restore_job(
    database: &Database,
    storage: &StorageRegistry,
    job: &launcher_database::RestoreJob,
) -> Result<()> {
    let (source_provider_id, bytes) = match storage
        .read_from_restore_source(&job.encoded_hash, StorageClass::Cold)
        .await
    {
        Ok(result) => result,
        Err(error) => {
            let error = error.to_string();

            database
                .fail_restore_job(job.id, &error, job.attempts < job.max_attempts)
                .await?;
            anyhow::bail!(error);
        }
    };
    let Some(hot_provider) = storage
        .provider(&job.target_provider)
        .filter(|provider| {
            storage
                .pool_for_provider(provider.provider_id())
                .is_some_and(|pool| pool.storage_class == StorageClass::Hot)
        })
        .or_else(|| {
            storage
                .providers_for_tier(StorageTier::Hot)
                .into_iter()
                .next()
        })
    else {
        let error = "no hot provider is configured for restore";
        database
            .fail_restore_job(job.id, error, job.attempts < job.max_attempts)
            .await?;
        anyhow::bail!(error);
    };
    if blake3::hash(&bytes).to_hex().as_str() != job.encoded_hash {
        let error = format!(
            "restore integrity verification failed for {}",
            job.encoded_hash
        );
        database
            .fail_restore_job(job.id, &error, job.attempts < job.max_attempts)
            .await?;
        anyhow::bail!(error);
    }
    let object_key = format!("chunks/encoded/{}.bin", job.encoded_hash);
    let hot_pool = storage
        .pool_for_provider(hot_provider.provider_id())
        .cloned()
        .with_context(|| {
            format!(
                "provider {} has no storage pool",
                hot_provider.provider_id()
            )
        })?;
    let source_pool = storage
        .pool_for_provider(&source_provider_id)
        .cloned()
        .with_context(|| format!("provider {source_provider_id} has no storage pool"))?;
    if let Err(error) = hot_provider.put_encoded(&job.encoded_hash, &bytes).await {
        let message = error.to_string();
        database
            .fail_restore_job(job.id, &message, job.attempts < job.max_attempts)
            .await?;
        return Err(error.into());
    }
    println!(
        "restore_source hash={} class={} pool={} failure_domain={} target_pool={} target_failure_domain={}",
        job.encoded_hash,
        source_pool.storage_class,
        source_pool.id,
        source_pool.failure_domain,
        hot_pool.id,
        hot_pool.failure_domain
    );
    database
        .add_storage_object_with_pool(
            &job.encoded_hash,
            i64::try_from(bytes.len())?,
            hot_provider.provider_id(),
            &hot_pool.id,
            &hot_pool.failure_domain,
            StorageTier::Hot,
            &object_key,
        )
        .await?;
    if let Ok(location) = hot_provider.download_location(&job.encoded_hash).await
        && location.expires_at.is_none()
    {
        database
            .add_storage_location_with_pool(
                &job.encoded_hash,
                hot_provider.provider_id(),
                &hot_pool.id,
                &hot_pool.failure_domain,
                StorageTier::Hot,
                &object_key,
                &location.url,
                0,
            )
            .await?;
    }
    database.complete_restore_job(job.id).await?;
    Ok(())
}

async fn renew_due_hot_packs(
    database: &Database,
    storage: &StorageRegistry,
    worker_id: &str,
    storage_root: &Path,
) -> Result<()> {
    let renewal_days = env::var("LAUNCHER_HOT_RENEWAL_DAYS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(18)
        .clamp(1, 30);
    let batch_size = env::var("LAUNCHER_HOT_RENEWAL_BATCH")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(4)
        .clamp(1, 32);
    let uploaded_before = Utc::now() - chrono::Duration::days(renewal_days);
    let renewal_root = storage_root.join("pack-renewal");
    tokio::fs::create_dir_all(&renewal_root).await?;

    // FileMirage's free retention is inactivity-based. Keep the provider
    // list explicit so a provider with a different retention contract is not
    // accidentally reuploaded on this schedule.
    let renewal_provider_types = env::var("LAUNCHER_HOT_RENEWAL_PROVIDER_TYPES")
        .unwrap_or_else(|_| "filemirage".to_owned())
        .split(',')
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect::<HashSet<_>>();
    for provider in storage.providers_for_tier(StorageTier::Hot) {
        if !renewal_provider_types.contains(&provider.provider_type().to_ascii_lowercase())
            || !provider.capabilities().upload
            || !provider.capabilities().direct_download
        {
            continue;
        }
        let due = database
            .list_due_hot_pack_renewals(provider.provider_id(), uploaded_before, batch_size)
            .await?;
        for location in due {
            let Some(lease_id) = database
                .acquire_pack_lease(&location.pack_hash, worker_id, 3600)
                .await?
            else {
                continue;
            };
            let temporary = renewal_root.join(format!(
                "{}.{}.pack.part",
                location.pack_hash,
                Uuid::new_v4()
            ));
            let result =
                renew_hot_pack_location(database, storage, provider.clone(), &location, &temporary)
                    .await;
            let _ = database.release_pack_lease(lease_id).await;
            let _ = tokio::fs::remove_file(&temporary).await;
            if let Err(error) = result {
                database
                    .defer_hot_pack_renewal(&location.pack_hash, &location.provider, 3600)
                    .await?;
                eprintln!(
                    "hot_pack_renewal pack_hash={} provider={} status=RETRY error={error}",
                    location.pack_hash, location.provider
                );
            } else {
                println!(
                    "hot_pack_renewal pack_hash={} provider={} status=DONE renewal_days={}",
                    location.pack_hash, location.provider, renewal_days
                );
            }
        }
    }
    Ok(())
}

async fn renew_hot_pack_location(
    database: &Database,
    storage: &StorageRegistry,
    target_provider: Arc<dyn StorageProvider>,
    location: &launcher_database::PackLocationRecord,
    temporary: &Path,
) -> Result<()> {
    let mut source_providers = vec![target_provider.clone()];
    source_providers.extend(
        storage
            .restore_sources(StorageClass::Cold)
            .into_iter()
            .filter(|provider| provider.provider_id() != target_provider.provider_id()),
    );
    let mut source_provider = None;
    let mut last_error = None;
    for provider in source_providers {
        match stream_verified_pack_to_file(&provider, &location.pack_hash, temporary).await {
            Ok(()) => {
                source_provider = Some(provider.provider_id().to_owned());
                break;
            }
            Err(error) => last_error = Some(error),
        }
    }
    let Some(source_provider) = source_provider else {
        return Err(last_error
            .unwrap_or_else(|| anyhow::anyhow!("no source was available for pack renewal")));
    };

    target_provider
        .put_pack_file(&location.pack_hash, temporary)
        .await
        .with_context(|| {
            format!(
                "could not reupload pack {} to {}",
                location.pack_hash,
                target_provider.provider_id()
            )
        })?;
    let runtime_location = target_provider
        .download_pack_location(&location.pack_hash)
        .await
        .context("renewed HOT pack did not produce a direct download location")?;
    if runtime_location.url.is_empty() {
        anyhow::bail!("renewed HOT pack returned an empty direct download location");
    }
    let hot_pool = storage
        .pool_for_provider(target_provider.provider_id())
        .cloned()
        .context("renewal provider has no HOT storage pool")?;
    let object_key = format!("packs/{}.pack", location.pack_hash);
    database
        .add_pack_location(
            &location.pack_hash,
            target_provider.provider_id(),
            &hot_pool.id,
            &hot_pool.failure_domain,
            StorageTier::Hot,
            &object_key,
            &runtime_location.url,
            hot_pool.priority,
            runtime_location.expires_at,
        )
        .await?;
    // The new link is recorded before stale database links are removed. The
    // provider's old remote object may not support deletion; if so it becomes
    // unreachable to Vaultnode and FileMirage's own inactivity expiry removes
    // it later.
    database
        .delete_pack_locations_except(
            &location.pack_hash,
            target_provider.provider_id(),
            &runtime_location.url,
        )
        .await?;
    println!(
        "hot_pack_renewal_source pack_hash={} source_provider={} target_provider={}",
        location.pack_hash,
        source_provider,
        target_provider.provider_id()
    );
    Ok(())
}

async fn stream_verified_pack_to_file(
    provider: &Arc<dyn StorageProvider>,
    pack_hash: &str,
    path: &Path,
) -> Result<()> {
    let mut stream = provider.read_pack_stream(pack_hash).await?;
    let mut output = tokio::fs::File::create(path).await?;
    let mut hasher = blake3::Hasher::new();
    let mut total = 0_u64;
    let max_bytes = env::var("LAUNCHER_PACK_MAX_BYTES")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(1024 * 1024 * 1024);
    while let Some(bytes) = stream.next().await {
        let bytes = bytes?;
        total = total
            .checked_add(bytes.len() as u64)
            .context("pack renewal size overflow")?;
        if total > max_bytes {
            anyhow::bail!("pack renewal exceeded LAUNCHER_PACK_MAX_BYTES");
        }
        hasher.update(&bytes);
        output.write_all(&bytes).await?;
    }
    output.flush().await?;
    if hasher.finalize().to_hex().as_str() != pack_hash {
        anyhow::bail!("pack renewal BLAKE3 verification failed for {pack_hash}");
    }
    let bytes = tokio::fs::read(path).await?;
    launcher_packs::PackReader::parse(&bytes)
        .and_then(|reader| reader.verify_pack_hash(pack_hash).map(|_| reader))
        .map_err(|error| anyhow::anyhow!("renewed pack is structurally invalid: {error}"))?;
    Ok(())
}

async fn process_pack_restore_job(
    database: &Database,
    storage: &StorageRegistry,
    job: &launcher_database::PackRestoreJob,
) -> Result<()> {
    let mut source_provider_id = None;
    let mut bytes = None;
    for provider in storage.restore_sources(StorageClass::Cold) {
        match provider.read_pack(&job.pack_hash).await {
            Ok(value) => {
                source_provider_id = Some(provider.provider_id().to_owned());
                bytes = Some(value);
                break;
            }
            Err(_) => continue,
        }
    }
    let Some(source_provider_id) = source_provider_id else {
        let error = format!("no COLD source contains pack {}", job.pack_hash);
        database
            .fail_pack_restore_job(job.id, &error, job.attempts < job.max_attempts)
            .await?;
        anyhow::bail!(error);
    };
    let bytes = bytes.expect("source bytes exist when source provider exists");
    if blake3::hash(&bytes).to_hex().as_str() != job.pack_hash {
        let error = format!(
            "pack restore integrity verification failed for {}",
            job.pack_hash
        );
        database
            .fail_pack_restore_job(job.id, &error, job.attempts < job.max_attempts)
            .await?;
        anyhow::bail!(error);
    }
    launcher_packs::PackReader::parse(&bytes)
        .and_then(|reader| reader.verify_pack_hash(&job.pack_hash).map(|_| reader))
        .map_err(|error| anyhow::anyhow!("restored pack is structurally invalid: {error}"))?;
    let Some(hot_provider) = storage
        .provider(&job.target_provider)
        .filter(|provider| {
            storage
                .pool_for_provider(provider.provider_id())
                .is_some_and(|pool| pool.storage_class == StorageClass::Hot)
        })
        .or_else(|| {
            storage
                .providers_for_tier(StorageTier::Hot)
                .into_iter()
                .next()
        })
    else {
        let error = "no hot provider is configured for pack restore";
        database
            .fail_pack_restore_job(job.id, error, job.attempts < job.max_attempts)
            .await?;
        anyhow::bail!(error);
    };
    let hot_pool = storage
        .pool_for_provider(hot_provider.provider_id())
        .cloned()
        .context("hot provider has no storage pool")?;
    if let Err(error) = hot_provider.put_pack(&job.pack_hash, &bytes).await {
        let message = error.to_string();
        database
            .fail_pack_restore_job(job.id, &message, job.attempts < job.max_attempts)
            .await?;
        return Err(error.into());
    }
    read_verified_pack_with_retry(&hot_provider, &job.pack_hash)
        .await
        .with_context(|| {
            format!(
                "restored HOT physical pack {} could not be read back and verified",
                job.pack_hash
            )
        })?;
    let object_key = format!("packs/{}.pack", job.pack_hash);
    let location = hot_provider
        .download_pack_location(&job.pack_hash)
        .await
        .ok();
    let direct_url = location
        .as_ref()
        .map(|value| value.url.clone())
        .unwrap_or_default();
    let expires_at = location.and_then(|value| value.expires_at);
    database
        .add_pack_location(
            &job.pack_hash,
            hot_provider.provider_id(),
            &hot_pool.id,
            &hot_pool.failure_domain,
            StorageTier::Hot,
            &object_key,
            &direct_url,
            hot_pool.priority,
            expires_at,
        )
        .await?;
    println!(
        "pack_restore_source pack_hash={} source_provider={} target_provider={}",
        job.pack_hash,
        source_provider_id,
        hot_provider.provider_id()
    );
    database.complete_pack_restore_job(job.id).await?;
    Ok(())
}

async fn read_verified_pack_with_retry(
    provider: &Arc<dyn StorageProvider>,
    pack_hash: &str,
) -> Result<Vec<u8>> {
    let mut last_error = None;
    for attempt in 0..4 {
        match provider.read_pack(pack_hash).await {
            Ok(bytes) => {
                if blake3::hash(&bytes).to_hex().as_str() != pack_hash {
                    last_error = Some(anyhow::anyhow!(
                        "HOT physical pack BLAKE3 verification failed"
                    ));
                } else if let Err(error) = launcher_packs::PackReader::parse(&bytes)
                    .and_then(|reader| reader.verify_pack_hash(pack_hash).map(|_| reader))
                {
                    last_error = Some(anyhow::anyhow!(
                        "HOT physical pack structure verification failed: {error}"
                    ));
                } else {
                    return Ok(bytes);
                }
            }
            Err(error) => last_error = Some(anyhow::anyhow!(error.to_string())),
        }
        if attempt < 3 {
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("HOT physical pack read failed")))
}

async fn publish_verified_build(
    manifest: &Manifest,
    manifest_bytes: &[u8],
    signature: &ManifestSignature,
    package: &Path,
    storage: &StorageRegistry,
    database: Option<&Database>,
) -> Result<()> {
    let policy = StoragePolicy::from_env().map_err(|error| anyhow::anyhow!(error))?;
    let packs_enabled = env_bool("PACK_STORAGE_ENABLED", false);
    let telegram_enabled = storage
        .providers()
        .iter()
        .any(|provider| provider.provider_type().eq_ignore_ascii_case("telegram"));
    if telegram_enabled && !packs_enabled {
        anyhow::bail!(
            "Telegram COLD retention requires PACK_STORAGE_ENABLED=true; Telegram stores immutable packs, not an untracked logical-chunk history"
        );
    }
    // Physical packs are the COLD contract whenever pack storage is enabled.
    // Keep the legacy flag as an explicit compatibility override, but default
    // it on so Telegram cannot silently receive logical-chunk replicas.
    let pack_cold_only = env_bool("LAUNCHER_PACK_COLD_ONLY", packs_enabled);
    if pack_cold_only && !packs_enabled {
        anyhow::bail!("LAUNCHER_PACK_COLD_ONLY=true requires PACK_STORAGE_ENABLED=true");
    }
    // Once pack storage is enabled, make the immutable pack the canonical
    // byte store by default. Logical chunks remain in the manifest/database
    // for FastCDC diffing and pack indexes, but are not uploaded as a second
    // set of HOT objects. Set LAUNCHER_PACK_CANONICAL=false only while
    // migrating an older deployment that still needs logical object URLs.
    let pack_canonical = packs_enabled && env_bool("LAUNCHER_PACK_CANONICAL", true);
    let pack_policy = if pack_cold_only {
        let mut pack_policy = policy.clone();
        // In pack mode, the immutable physical pack is the COLD replication
        // unit. This makes an accidentally incomplete COLD configuration fail
        // before a build can be published without a cold copy.
        pack_policy.min_verified_cold_replicas = pack_policy.min_verified_cold_replicas.max(1);
        pack_policy.preferred_cold_replicas = pack_policy.preferred_cold_replicas.max(1);
        pack_policy.min_cold_failure_domains = pack_policy.min_cold_failure_domains.max(1);
        pack_policy.cold_backup_required = true;
        pack_policy
    } else {
        policy.clone()
    };
    if let Some(database) = database {
        database
            .upsert_game(&GameSummary {
                id: manifest.game_id.clone(),
                slug: manifest.game_id.clone(),
                title: manifest.game_id.clone(),
                description: "Published launcher build".to_owned(),
                hero_image_url: None,
                cover_image_url: None,
                latest_build: None,
            })
            .await?;
        database
            .upsert_build_with_bytes(manifest, manifest_bytes, Some(signature), "READY")
            .await?;
        for file in &manifest.files {
            for (ordinal, chunk) in file.chunks.iter().enumerate() {
                database
                    .add_chunk(
                        &chunk.encoded_hash,
                        i64::try_from(chunk.encoded_size)?,
                        &manifest.encoding.id,
                    )
                    .await?;
                database
                    .attach_build_chunk(
                        &manifest.build_id,
                        &chunk.encoded_hash,
                        i64::try_from(chunk.raw_size)?,
                        &chunk.raw_hash,
                        i32::try_from(ordinal)?,
                    )
                    .await?;
            }
        }
    }

    if !pack_canonical {
        let logical_policy = if pack_cold_only {
            let logical_hot_replicas = env::var("LAUNCHER_LOGICAL_HOT_REPLICAS")
                .ok()
                .and_then(|value| value.parse::<u32>().ok())
                .unwrap_or(1);
            let mut logical_policy = logical_pack_policy(&policy, logical_hot_replicas)?;
            logical_policy.min_verified_cold_replicas = 0;
            logical_policy.preferred_cold_replicas = 0;
            logical_policy.min_cold_failure_domains = 0;
            logical_policy.cold_backup_required = false;
            logical_policy
        } else {
            policy.clone()
        };
        let placement_engine =
            StoragePlacementEngine::new(logical_policy).map_err(|error| anyhow::anyhow!(error))?;
        let placement_pools = storage.placement_pools().await;
        let mut uploaded = HashSet::new();
        let chunks = manifest
            .files
            .iter()
            .flat_map(|file| file.chunks.iter())
            .filter(|chunk| uploaded.insert(chunk.encoded_hash.clone()))
            .cloned()
            .collect::<Vec<_>>();
        let publish_concurrency = env::var("LAUNCHER_PUBLISH_CONCURRENCY")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(8)
            .clamp(1, 32);
        let mut tasks = tokio::task::JoinSet::new();
        for chunk in chunks {
            while tasks.len() >= publish_concurrency {
                let _ = tasks
                    .join_next()
                    .await
                    .context("logical chunk publish task disappeared")??;
            }
            let package = package.to_owned();
            let storage = storage.clone();
            let database = database.cloned();
            let placement_engine = placement_engine.clone();
            let placement_pools = placement_pools.clone();
            let build_id = manifest.build_id.clone();
            tasks.spawn(async move {
                publish_logical_chunk(
                    &chunk,
                    &build_id,
                    &package,
                    &storage,
                    database.as_ref(),
                    &placement_engine,
                    &placement_pools,
                )
                .await
            });
        }
        while let Some(result) = tasks.join_next().await {
            result.context("logical chunk publish task failed")??;
        }
    }
    if packs_enabled && let Some(database) = database {
        publish_physical_packs(&manifest.build_id, package, storage, database, &pack_policy)
            .await?;
    }
    if let Some(database) = database {
        if pack_canonical {
            if !database
                .build_packs_cover_all_chunks(&manifest.build_id)
                .await?
            {
                anyhow::bail!(
                    "pack-canonical build {} has manifest chunks missing from its physical-pack index",
                    manifest.build_id
                );
            }
            database
                .publish_build_with_pack_storage_policy(&manifest.build_id, &pack_policy)
                .await?;
        } else {
            database
                .publish_build_with_storage_policy(&manifest.build_id, &policy)
                .await?;
        }
        // Legacy logical objects are left for a separate cleanup pass in
        // pack-canonical mode. This keeps publication independent of whether
        // an old HOT adapter supports deletion.
        retire_superseded_hot_storage(
            &manifest.game_id,
            &manifest.build_id,
            storage,
            database,
            !pack_canonical,
        )
        .await?;
    }
    Ok(())
}

async fn retire_superseded_hot_storage(
    game_id: &str,
    latest_build_id: &str,
    storage: &StorageRegistry,
    database: &Database,
    retire_logical_objects: bool,
) -> Result<()> {
    let objects = if retire_logical_objects {
        database
            .list_hot_objects_to_retire(game_id, latest_build_id)
            .await?
    } else {
        Vec::new()
    };
    let packs = database
        .list_hot_pack_locations_to_retire(game_id, latest_build_id)
        .await?;
    let mut retired_objects = 0_u64;
    let mut retired_packs = 0_u64;
    let mut skipped_providers = HashSet::new();

    for object in objects {
        let Some(provider) = storage.provider(&object.provider) else {
            skipped_providers.insert(object.provider);
            continue;
        };
        if !provider.capabilities().delete {
            skipped_providers.insert(object.provider.clone());
            continue;
        }
        provider
            .delete_encoded(&object.encoded_hash)
            .await
            .with_context(|| {
                format!(
                    "could not retire old HOT object {} from {}",
                    object.encoded_hash,
                    provider.provider_id()
                )
            })?;
        database
            .delete_storage_object(&object.encoded_hash, &object.provider)
            .await?;
        retired_objects += 1;
    }

    for location in packs {
        let Some(provider) = storage.provider(&location.provider) else {
            skipped_providers.insert(location.provider);
            continue;
        };
        if !provider.capabilities().delete {
            skipped_providers.insert(location.provider.clone());
            continue;
        }
        provider
            .delete_pack(&location.pack_hash)
            .await
            .with_context(|| {
                format!(
                    "could not retire old HOT pack {} from {}",
                    location.pack_hash,
                    provider.provider_id()
                )
            })?;
        database
            .delete_pack_location(
                &location.pack_hash,
                &location.provider,
                &location.direct_url,
            )
            .await?;
        retired_packs += 1;
    }

    if skipped_providers.is_empty() {
        println!(
            "hot_retention=APPLIED game={} latest_build={} objects={} packs={}",
            game_id, latest_build_id, retired_objects, retired_packs
        );
    } else {
        println!(
            "hot_retention=PARTIAL game={} latest_build={} objects={} packs={} providers_not_configured={:?}",
            game_id, latest_build_id, retired_objects, retired_packs, skipped_providers
        );
    }
    Ok(())
}

fn logical_pack_policy(policy: &StoragePolicy, logical_hot_replicas: u32) -> Result<StoragePolicy> {
    if logical_hot_replicas == 0 {
        anyhow::bail!("LAUNCHER_LOGICAL_HOT_REPLICAS must be at least 1");
    }
    let mut logical_policy = policy.clone();
    logical_policy.min_verified_hot_replicas = logical_hot_replicas;
    logical_policy.preferred_hot_replicas = logical_hot_replicas;
    logical_policy.min_hot_failure_domains = logical_policy
        .min_hot_failure_domains
        .min(logical_hot_replicas);
    logical_policy
        .validate()
        .map_err(|error| anyhow::anyhow!(error))?;
    Ok(logical_policy)
}

async fn publish_logical_chunk(
    chunk: &ChunkRef,
    build_id: &str,
    package: &Path,
    storage: &StorageRegistry,
    database: Option<&Database>,
    placement_engine: &StoragePlacementEngine,
    placement_pools: &[launcher_storage::StoragePoolCandidate],
) -> Result<()> {
    let object_path = package
        .join(&chunk.object_key)
        .canonicalize()
        .with_context(|| format!("could not resolve {}", chunk.object_key))?;
    let bytes = std::fs::read(&object_path)
        .with_context(|| format!("could not read {}", object_path.display()))?;
    if blake3::hash(&bytes).to_hex().as_str() != chunk.encoded_hash {
        anyhow::bail!("storage object hash mismatch: {}", chunk.encoded_hash);
    }
    let existing_objects = if let Some(database) = database {
        database
            .list_storage_objects(std::slice::from_ref(&chunk.encoded_hash))
            .await?
    } else {
        Vec::new()
    };
    let existing_replicas = existing_objects
        .iter()
        .filter(|object| object.verified_at.is_some())
        .filter_map(|object| {
            let pool = storage
                .pool_for_provider(&object.provider)
                .or_else(|| storage.pool(&object.pool_id))?;
            Some(ExistingStorageReplica {
                provider_id: object.provider.clone(),
                pool_id: pool.id.clone(),
                storage_class: object.tier,
                failure_domain: if object.failure_domain.is_empty() {
                    pool.failure_domain.clone()
                } else {
                    object.failure_domain.clone()
                },
            })
        })
        .collect::<Vec<_>>();
    let plan =
        placement_engine.plan_with_pools(bytes.len() as u64, &existing_replicas, placement_pools);
    if !plan.policy_satisfied {
        let cold_capacity_needed = plan.projected_cold_replicas < plan.required_cold_replicas
            || plan.projected_cold_failure_domains < plan.required_cold_failure_domains;
        if cold_capacity_needed
            && let Some(database) = database
            && let Some(candidate) = placement_pools.iter().find(|candidate| {
                candidate.storage_class == StorageClass::Cold
                    && storage.pool(&candidate.pool_id).is_some_and(|pool| {
                        !matches!(
                            pool.provisioning_mode,
                            launcher_storage::ProvisioningMode::Disabled
                        )
                    })
            })
        {
            let manager = provisioning_manager(database);
            let headroom = env::var("PROVISIONING_CAPACITY_HEADROOM_BYTES")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0);
            let request = ProvisionRequest {
                provider_type: candidate.provider_type.clone(),
                pool_id: candidate.pool_id.clone(),
                requested_capacity_bytes: (bytes.len() as u64).saturating_add(headroom),
                expires_at: Utc::now() + chrono::Duration::hours(24),
                idempotency_key: format!("publication:{}:{}", build_id, candidate.pool_id),
            };
            let job = manager
                .ensure_capacity(request)
                .await
                .map_err(provisioning_anyhow)?;
            anyhow::bail!(
                "storage policy is blocked on capacity provisioning for {}: job={} status={}",
                chunk.encoded_hash,
                job.id,
                job.status
            );
        }
        anyhow::bail!(
            "storage policy cannot be satisfied for {}: {}",
            chunk.encoded_hash,
            plan.explanation
        );
    }
    for action in plan.actions {
        let provider = storage.provider(&action.provider_id).ok_or_else(|| {
            anyhow::anyhow!("storage provider {} is not configured", action.provider_id)
        })?;
        provider
            .put_encoded(&chunk.encoded_hash, &bytes)
            .await
            .with_context(|| {
                format!(
                    "could not upload {} to {} ({})",
                    chunk.encoded_hash, action.provider_id, action.tier
                )
            })?;
        if let Some(database) = database {
            let encoded_size = i64::try_from(bytes.len())?;
            database
                .add_storage_object_with_pool(
                    &chunk.encoded_hash,
                    encoded_size,
                    provider.provider_id(),
                    &action.pool_id,
                    &action.failure_domain,
                    action.tier,
                    &chunk.object_key,
                )
                .await?;
            if action.tier == StorageTier::Hot
                && let Ok(location) = provider.download_location(&chunk.encoded_hash).await
                && location.expires_at.is_none()
            {
                database
                    .add_storage_location_with_pool(
                        &chunk.encoded_hash,
                        provider.provider_id(),
                        &action.pool_id,
                        &action.failure_domain,
                        action.tier,
                        &chunk.object_key,
                        &location.url,
                        action.priority,
                    )
                    .await?;
            }
        }
    }
    Ok(())
}

async fn publish_physical_packs(
    build_id: &str,
    package: &Path,
    storage: &StorageRegistry,
    database: &Database,
    policy: &StoragePolicy,
) -> Result<()> {
    let packs_dir = package.join("packs");
    if !packs_dir.is_dir() {
        anyhow::bail!(
            "PACK_STORAGE_ENABLED is true but package has no packs directory: {}",
            packs_dir.display()
        );
    }
    let pack_config = PackConfig::from_env().map_err(|error| anyhow::anyhow!(error))?;
    let placement_engine =
        StoragePlacementEngine::new(policy.clone()).map_err(|error| anyhow::anyhow!(error))?;
    let placement_pools = storage.placement_pools().await;
    let mut paths = walkdir::WalkDir::new(&packs_dir)
        .follow_links(false)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry.file_type().is_file()
                && entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "pack")
        })
        .map(|entry| entry.path().to_owned())
        .collect::<Vec<_>>();
    paths.sort();
    for path in paths {
        let pack_hash = path
            .file_stem()
            .and_then(|value| value.to_str())
            .context("pack filename is not valid UTF-8")?;
        let bytes = std::fs::read(&path)?;
        let pack_size = bytes.len();
        let actual_hash = blake3::hash(&bytes).to_hex().to_string();
        if actual_hash != pack_hash {
            anyhow::bail!(
                "pack filename hash mismatch for {}: expected {}, got {}",
                path.display(),
                pack_hash,
                actual_hash
            );
        }
        let reader = launcher_packs::PackReader::parse(&bytes)
            .map_err(|error| anyhow::anyhow!("invalid pack {}: {error}", path.display()))?;
        reader
            .verify_pack_hash(pack_hash)
            .map_err(|error| anyhow::anyhow!("pack identity verification failed: {error}"))?;
        let entries = reader.entries().to_vec();
        drop(reader);
        drop(bytes);
        database
            .upsert_physical_pack(
                pack_hash,
                1,
                i64::try_from(pack_size)?,
                i64::try_from(entries.len())?,
                i64::try_from(pack_config.target_bytes)?,
                "UPLOADING",
            )
            .await?;
        database.attach_build_pack(build_id, pack_hash).await?;
        for entry in &entries {
            database
                .add_pack_chunk(
                    pack_hash,
                    &entry.encoded_hash,
                    &entry.raw_hash,
                    i64::try_from(entry.raw_length)?,
                    i64::try_from(entry.offset)?,
                    i64::try_from(entry.encoded_length)?,
                    "zstd-v1",
                    i32::try_from(entry.flags)?,
                )
                .await?;
        }
        let plan = placement_engine.plan_with_pools(pack_size as u64, &[], &placement_pools);
        if !plan.policy_satisfied {
            anyhow::bail!(
                "storage policy cannot be satisfied for pack {}: {}",
                pack_hash,
                plan.explanation
            );
        }
        let object_key = format!("packs/{pack_hash}.pack");
        for action in plan.actions {
            let provider = storage.provider(&action.provider_id).ok_or_else(|| {
                anyhow::anyhow!("storage provider {} is not configured", action.provider_id)
            })?;
            provider
                .put_pack_file(pack_hash, &path)
                .await
                .with_context(|| {
                    format!(
                        "could not upload pack {pack_hash} to {}",
                        provider.provider_id()
                    )
                })?;
            let location = provider.download_pack_location(pack_hash).await.ok();
            let direct_url = location
                .as_ref()
                .map(|value| value.url.clone())
                .unwrap_or_default();
            let expires_at = location.and_then(|value| value.expires_at);
            database
                .add_pack_location(
                    pack_hash,
                    provider.provider_id(),
                    &action.pool_id,
                    &action.failure_domain,
                    action.tier,
                    &object_key,
                    &direct_url,
                    action.priority,
                    expires_at,
                )
                .await?;
        }
        database
            .upsert_physical_pack(
                pack_hash,
                1,
                i64::try_from(pack_size)?,
                i64::try_from(entries.len())?,
                i64::try_from(pack_config.target_bytes)?,
                "VERIFIED",
            )
            .await?;
    }
    Ok(())
}

fn atomic_copy(source: &std::path::Path, destination: &std::path::Path) -> Result<()> {
    let temporary = destination.with_extension("part");
    std::fs::copy(source, &temporary)?;
    std::fs::rename(temporary, destination)?;
    Ok(())
}

fn copy_verified_objects(source: &std::path::Path, destination: &std::path::Path) -> Result<()> {
    if !source.is_dir() {
        anyhow::bail!(
            "package has no encoded object directory: {}",
            source.display()
        );
    }
    for entry in walkdir::WalkDir::new(source).follow_links(false) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy();
        if !name.ends_with(".bin") {
            anyhow::bail!("unexpected storage object name: {name}");
        }
        let hash = name.trim_end_matches(".bin");
        if hash.len() != 64
            || !hash
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            anyhow::bail!("invalid storage object hash: {hash}");
        }
        let bytes = std::fs::read(entry.path())?;
        if blake3::hash(&bytes).to_hex().as_str() != hash {
            anyhow::bail!("storage object hash mismatch: {hash}");
        }
        let target = destination.join(name.as_ref());
        if target.exists() {
            let existing = std::fs::read(&target)?;
            if blake3::hash(&existing).to_hex().as_str() != hash {
                anyhow::bail!("existing storage object is corrupt: {hash}");
            }
        } else {
            atomic_copy(entry.path(), &target)?;
        }
    }
    Ok(())
}
