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
use launcher_storage::{StorageRegistry, storage_from_env};
use launcher_worker::IngestionProgress;
use std::process::Command;
use std::{
    collections::HashSet,
    env,
    path::{Path, PathBuf},
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
            let base_url = env::var("LAUNCHER_PUBLIC_BASE_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:8080".to_owned());
            let (storage, _) = storage_from_env(&storage_root, base_url)?;
            let database = if let Ok(url) = env::var("DATABASE_URL") {
                let database = Database::connect(&url).await?;
                if env::var("LAUNCHER_AUTO_MIGRATE").as_deref() == Ok("1") {
                    database.migrate().await?;
                }
                Some(database)
            } else {
                None
            };
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

async fn publish_verified_build(
    manifest: &Manifest,
    signature: &ManifestSignature,
    package: &Path,
    storage: &StorageRegistry,
    database: Option<&Database>,
) -> Result<()> {
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
            storage.put_encoded(&chunk.encoded_hash, &bytes).await?;
            if let Some(database) = database {
                let encoded_size = i64::try_from(bytes.len())?;
                for (priority, provider) in storage.providers().iter().enumerate() {
                    database
                        .add_storage_object(
                            &chunk.encoded_hash,
                            encoded_size,
                            provider.provider_id(),
                            &chunk.object_key,
                        )
                        .await?;
                    let location = provider.download_location(&chunk.encoded_hash).await?;
                    if location.expires_at.is_none() {
                        database
                            .add_storage_location(
                                &chunk.encoded_hash,
                                provider.provider_id(),
                                &chunk.object_key,
                                &location.url,
                                i32::try_from(priority)?,
                            )
                            .await?;
                    }
                }
            }
        }
    }
    if let Some(database) = database {
        database.publish_build(&manifest.build_id).await?;
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
