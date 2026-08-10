use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use clap::{Parser, Subcommand};
use launcher_common::{GameSummary, Manifest, ManifestSignature};
use launcher_database::Database;
use launcher_domain::BuildState;
use launcher_manifests::{
    generate_signing_key, load_private_key_pem, private_key_pem, public_key_pem, sign_bytes,
    validate_json, verify_bytes,
};
use launcher_packager::{PackageOptions, package_directory};
use launcher_storage::{
    CapacityReservationStore, InMemoryCapacityReservationStore, MegaAccountBackend,
    MegaAccountConfig, MegaCliAccount, MegaColdStorageConfig, PlacementProvider,
    StorageAccountStatus, StoragePlacementEngine, StoragePolicy, StorageProvider, StorageRegistry,
    StorageTier, storage_from_env_with_reservation_store,
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

#[derive(Debug, Parser)]
#[command(
    name = "launcher-admin",
    about = "Safe operator commands for authorized launcher content"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
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
    Smoke {
        #[arg(long, default_value = "hot")]
        provider: String,
        #[arg(long, default_value = "storage")]
        storage_root: PathBuf,
        #[arg(long, default_value_t = 32 * 1024)]
        bytes: usize,
        #[arg(long)]
        skip_download_url: bool,
    },
    MegaSmoke {
        #[arg(long, default_value = "storage")]
        storage_root: PathBuf,
        #[arg(long, default_value_t = 32 * 1024)]
        bytes: usize,
    },
    Health {
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
    Gc {
        #[arg(long)]
        dry_run: bool,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
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
            publish_verified_build(&manifest, &signature, &package, &storage, database.as_ref())
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
            let analysis_path = output.join("analysis.json");
            std::fs::create_dir_all(&output)?;
            let status = Command::new("python")
                .args(["-m", "launcher_analyzer", "analyze"])
                .arg(&input)
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
                &input,
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

    fetch_success(&client, &base_url.join("v1/health")?, "api_liveness").await?;
    fetch_success(&client, &base_url.join("v1/ready")?, "api_readiness").await?;
    let storage_status = fetch_json(
        &client,
        &base_url.join("api/v1/storage/status")?,
        "storage_status",
    )
    .await?;
    fetch_success(&client, &base_url.join("metrics")?, "metrics").await?;

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
    let response = client.get(url.clone()).send().await?;
    if !response.status().is_success() {
        anyhow::bail!("staging check {name} failed: HTTP {}", response.status())
    }
    println!("check={name} status=PASS http={}", response.status());
    Ok(())
}

async fn fetch_json(
    client: &reqwest::Client,
    url: &reqwest::Url,
    name: &str,
) -> Result<serde_json::Value> {
    let response = client.get(url.clone()).send().await?;
    if !response.status().is_success() {
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
    let mut url = base_url.join("api/v1/builds/")?;
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
    Ok((storage, database))
}

async fn handle_storage_command(command: StorageCommands) -> Result<()> {
    match command {
        StorageCommands::Policy => {
            let policy = StoragePolicy::from_env().map_err(|error| anyhow::anyhow!(error))?;
            println!("{}", serde_json::to_string_pretty(&policy)?);
        }
        StorageCommands::Smoke {
            provider,
            storage_root,
            bytes,
            skip_download_url,
        } => {
            let (storage, _) = storage_command_context(&storage_root).await?;
            let provider = select_provider(&storage, &provider, StorageTier::Hot)?;
            if provider.tier() != StorageTier::Hot {
                anyhow::bail!(
                    "storage smoke requires a HOT provider; use storage mega-smoke for COLD"
                )
            }
            run_storage_smoke(provider, bytes, !skip_download_url, "HOT").await?;
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
            if let Err(error) = run_storage_smoke(provider, bytes, false, "COLD").await {
                println!("diagnostic={}", mega_diagnostic(&error));
                return Err(error);
            }
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
        StorageCommands::Readiness { storage_root } => {
            let policy = StoragePolicy::from_env().map_err(|error| anyhow::anyhow!(error))?;
            let (storage, database) = storage_command_context(&storage_root).await?;
            let health = storage.health().await;
            let hot_healthy = health
                .iter()
                .filter(|provider| provider.tier == StorageTier::Hot && provider.healthy)
                .count() as u32;
            let cold_healthy = health
                .iter()
                .filter(|provider| provider.tier == StorageTier::Cold && provider.healthy)
                .count() as u32;
            if hot_healthy < policy.required_replicas(StorageTier::Hot) {
                anyhow::bail!(
                    "staging readiness failed: healthy hot providers {hot_healthy} below required {}",
                    policy.required_replicas(StorageTier::Hot)
                );
            }
            if cold_healthy < policy.required_replicas(StorageTier::Cold) {
                anyhow::bail!(
                    "staging readiness failed: healthy cold providers {cold_healthy} below required {}",
                    policy.required_replicas(StorageTier::Cold)
                );
            }
            if policy.required_replicas(StorageTier::Cold) > 0 {
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
                "readiness=READY hot_healthy={} cold_healthy={} required_hot={} required_cold={}",
                hot_healthy,
                cold_healthy,
                policy.required_replicas(StorageTier::Hot),
                policy.required_replicas(StorageTier::Cold)
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
            println!(
                "restore_worker=STARTED worker_id={worker_id} poll_seconds={}",
                poll.as_secs()
            );
            loop {
                database.recover_expired_restore_jobs().await?;
                if let Some(job) = database.claim_restore_job(&worker_id, 600).await? {
                    if let Err(error) = process_restore_job(&database, &storage, &job).await {
                        eprintln!("restore_job={} status=RETRY error={error}", job.id);
                    } else {
                        println!(
                            "restore_job={} status=DONE hash={}",
                            job.id, job.encoded_hash
                        );
                    }
                } else {
                    tokio::time::sleep(poll).await;
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
    tier_label: &str,
) -> Result<()> {
    let byte_count = requested_bytes.clamp(1, 4 * 1024 * 1024);
    let mut bytes = vec![0_u8; byte_count];
    OsRng.fill_bytes(&mut bytes);
    let encoded_hash = blake3::hash(&bytes).to_hex().to_string();
    let provider_id = provider.provider_id().to_owned();
    let result = async {
        provider.health_check().await?;
        println!("check={tier_label}_health status=PASS provider={provider_id}");
        provider.put_encoded(&encoded_hash, &bytes).await?;
        println!(
            "check={tier_label}_put status=PASS bytes={} hash={encoded_hash}",
            bytes.len()
        );
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
        let location = provider.download_location(&encoded_hash).await?;
        if fetch_download_url {
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
        }
        println!(
            "check={tier_label}_download_url status=PASS expires_at={}",
            location
                .expires_at
                .map(|value| value.to_rfc3339())
                .unwrap_or_else(|| "none".to_owned())
        );
        Ok::<(), anyhow::Error>(())
    }
    .await;
    let cleanup = provider.delete_encoded(&encoded_hash).await;
    match result {
        Ok(()) => {
            cleanup?;
            if provider.head_encoded(&encoded_hash).await?.is_some() {
                anyhow::bail!("smoke DELETE did not remove the temporary object");
            }
            println!("check={tier_label}_delete status=PASS");
            println!(
                "storage_smoke=PASS provider={provider_id} bytes={}",
                bytes.len()
            );
            Ok(())
        }
        Err(error) => {
            let _ = cleanup;
            Err(error)
        }
    }
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
        .providers_for_tier(StorageTier::Cold)
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

async fn connect_database() -> Result<Database> {
    let url = env::var("DATABASE_URL")
        .context("DATABASE_URL is required for this storage admin command")?;
    Ok(Database::connect(&url).await?)
}

async fn command_database() -> Result<Database> {
    let database = connect_database().await?;
    database.migrate().await?;
    Ok(database)
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
                "account={} provider={} status={} credential_reference={} capacity_bytes={}",
                persisted_config.account_id,
                provider_id,
                status.as_str(),
                persisted_config.credential_reference,
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
                    "account={} provider={} status={} credential_reference={} capacity={} used={} reserved={} available={}",
                    record.snapshot.account_id,
                    record.snapshot.provider_id,
                    record.snapshot.status.as_str(),
                    record.credential_reference,
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
                "account={} provider={} tier={} status={} credential_reference={} capacity={} used={} reserved={} available={} last_capacity_check={:?}",
                record.snapshot.account_id,
                record.snapshot.provider_id,
                record.snapshot.tier,
                record.snapshot.status.as_str(),
                record.credential_reference,
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
    let mut bytes = None;
    let mut last_error = None;
    for provider in storage.providers_for_tier(StorageTier::Cold) {
        match provider.read_encoded(&job.encoded_hash).await {
            Ok(value) => {
                bytes = Some(value);
                break;
            }
            Err(error) => last_error = Some(error),
        }
    }
    let bytes = if let Some(bytes) = bytes {
        bytes
    } else {
        let error = last_error
            .map(|error| error.to_string())
            .unwrap_or_else(|| "no cold provider is configured".to_owned());
        database
            .fail_restore_job(job.id, &error, job.attempts < job.max_attempts)
            .await?;
        anyhow::bail!(error);
    };
    let Some(hot_provider) = storage
        .provider(&job.target_provider)
        .filter(|provider| provider.tier() == StorageTier::Hot)
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
    if let Err(error) = hot_provider.put_encoded(&job.encoded_hash, &bytes).await {
        let message = error.to_string();
        database
            .fail_restore_job(job.id, &message, job.attempts < job.max_attempts)
            .await?;
        return Err(error.into());
    }
    let object_key = format!("chunks/encoded/{}.bin", job.encoded_hash);
    database
        .add_storage_object_with_tier(
            &job.encoded_hash,
            i64::try_from(bytes.len())?,
            hot_provider.provider_id(),
            StorageTier::Hot,
            &object_key,
        )
        .await?;
    if let Ok(location) = hot_provider.download_location(&job.encoded_hash).await
        && location.expires_at.is_none()
    {
        database
            .add_storage_location_with_tier(
                &job.encoded_hash,
                hot_provider.provider_id(),
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

async fn publish_verified_build(
    manifest: &Manifest,
    signature: &ManifestSignature,
    package: &Path,
    storage: &StorageRegistry,
    database: Option<&Database>,
) -> Result<()> {
    let policy = StoragePolicy::from_env().map_err(|error| anyhow::anyhow!(error))?;
    let placement_engine =
        StoragePlacementEngine::new(policy.clone()).map_err(|error| anyhow::anyhow!(error))?;
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
            .upsert_build(manifest, Some(signature), "READY")
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

    let mut placement_providers = Vec::with_capacity(storage.providers().len());
    for provider in storage.providers() {
        placement_providers.push(PlacementProvider {
            provider_id: provider.provider_id().to_owned(),
            tier: provider.tier(),
            healthy: provider.health_check().await.is_ok(),
            capacity_available_bytes: None,
        });
    }
    let mut uploaded = HashSet::new();
    for file in &manifest.files {
        for chunk in &file.chunks {
            if !uploaded.insert(chunk.encoded_hash.clone()) {
                continue;
            }
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
            let existing_provider_ids = existing_objects
                .iter()
                .filter(|object| object.verified_at.is_some())
                .map(|object| object.provider.clone())
                .collect::<Vec<_>>();
            let plan = placement_engine.plan(
                bytes.len() as u64,
                &existing_provider_ids,
                &placement_providers,
            );
            if !plan.policy_satisfied {
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
                        .add_storage_object_with_tier(
                            &chunk.encoded_hash,
                            encoded_size,
                            provider.provider_id(),
                            action.tier,
                            &chunk.object_key,
                        )
                        .await?;
                    if action.tier == StorageTier::Hot
                        && let Ok(location) = provider.download_location(&chunk.encoded_hash).await
                        && location.expires_at.is_none()
                    {
                        database
                            .add_storage_location_with_tier(
                                &chunk.encoded_hash,
                                provider.provider_id(),
                                action.tier,
                                &chunk.object_key,
                                &location.url,
                                i32::try_from(
                                    storage
                                        .providers()
                                        .iter()
                                        .position(|candidate| {
                                            candidate.provider_id() == provider.provider_id()
                                        })
                                        .unwrap_or_default(),
                                )?,
                            )
                            .await?;
                    }
                }
            }
        }
    }
    if let Some(database) = database {
        database
            .publish_build_with_policy(
                &manifest.build_id,
                policy.required_replicas(StorageTier::Hot),
                policy.required_replicas(StorageTier::Cold),
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
