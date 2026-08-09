use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use clap::{Parser, Subcommand};
use launcher_common::{GameSummary, Manifest, ManifestSignature};
use launcher_database::Database;
use launcher_domain::BuildState;
use launcher_manifests::{
    generate_signing_key, load_private_key_pem, sign_bytes, validate_json, verify_bytes,
};
use launcher_packager::{PackageOptions, package_directory};
use launcher_storage::{
    CapacityReservationStore, InMemoryCapacityReservationStore, MegaAccountBackend,
    MegaAccountConfig, MegaCliAccount, MegaColdStorageConfig, PlacementProvider,
    StorageAccountStatus, StoragePlacementEngine, StoragePolicy, StorageRegistry, StorageTier,
    storage_from_env_with_reservation_store,
};
use launcher_worker::IngestionProgress;
use std::process::Command;
use std::{
    collections::HashSet,
    env,
    path::{Path, PathBuf},
    sync::Arc,
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
    Storage {
        #[command(subcommand)]
        command: StorageCommands,
    },
}

#[derive(Debug, Subcommand)]
enum StorageCommands {
    Policy,
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
        Commands::ManifestSign {
            path,
            output,
            key_id,
            private_key,
        } => {
            let bytes = std::fs::read(&path)
                .with_context(|| format!("could not read {}", path.display()))?;
            validate_json(&bytes).map_err(|error| anyhow::anyhow!(error))?;
            let key = match private_key {
                Some(path) => {
                    let pem = std::fs::read_to_string(&path)
                        .with_context(|| format!("could not read {}", path.display()))?;
                    load_private_key_pem(&pem)?
                }
                None => generate_signing_key()?,
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

async fn command_database() -> Result<Database> {
    let url = env::var("DATABASE_URL")
        .context("DATABASE_URL is required for this storage admin command")?;
    let database = Database::connect(&url).await?;
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
