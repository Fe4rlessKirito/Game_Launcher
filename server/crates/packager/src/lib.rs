use anyhow::{Context, Result};
use chrono::Utc;
use fastcdc::v2020::StreamCDC;
use launcher_common::{
    ChunkRef, ChunkingConfig, EncodingConfig, FileRecipe, LaunchProfile, MANIFEST_SCHEMA_VERSION,
    Manifest,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::BufReader;
use std::path::Path;
use uuid::Uuid;
use walkdir::WalkDir;

#[derive(Debug, Clone)]
pub struct PackageOptions {
    pub game_id: String,
    pub build_id: String,
    pub display_version: String,
    pub executable: Option<String>,
    pub chunking: ChunkingConfig,
    pub encoding: EncodingConfig,
}

impl Default for PackageOptions {
    fn default() -> Self {
        Self {
            game_id: "synthetic-game".to_owned(),
            build_id: format!("build-{}", Utc::now().format("%Y%m%d%H%M%S")),
            display_version: "0.1.0".to_owned(),
            executable: None,
            chunking: ChunkingConfig::default(),
            encoding: EncodingConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PackageReport {
    pub manifest_id: String,
    pub files: u64,
    pub raw_bytes: u64,
    pub encoded_bytes: u64,
    pub chunks: u64,
    pub unique_chunks: u64,
    pub reused_chunks: u64,
    pub warnings: Vec<String>,
}

pub fn package_directory(
    input: impl AsRef<Path>,
    output: impl AsRef<Path>,
    options: &PackageOptions,
) -> Result<PackageReport> {
    if options.chunking.minimum_bytes < 64
        || options.chunking.minimum_bytes > 1_048_576
        || options.chunking.average_bytes < 256
        || options.chunking.average_bytes > 4_194_304
        || options.chunking.maximum_bytes < 1_024
        || options.chunking.maximum_bytes > 16_777_216
        || options.chunking.minimum_bytes > options.chunking.average_bytes
        || options.chunking.average_bytes > options.chunking.maximum_bytes
    {
        anyhow::bail!(
            "FastCDC 4.0 parameters must be within 64..1 MiB, 256..4 MiB, and 1 KiB..16 MiB"
        );
    }
    let input = input.as_ref().canonicalize().with_context(|| {
        format!(
            "input directory does not exist: {}",
            input.as_ref().display()
        )
    })?;
    if !input.is_dir() {
        anyhow::bail!("input is not a directory: {}", input.display());
    }
    let output = output.as_ref();
    fs::create_dir_all(output.join("chunks/encoded"))?;

    let mut files = Vec::new();
    let mut raw_bytes = 0_u64;
    let mut encoded_bytes = 0_u64;
    let mut chunks = 0_u64;
    let mut unique_chunks = 0_u64;
    let mut reused_chunks = 0_u64;
    let mut warnings = Vec::new();

    let mut paths = WalkDir::new(&input)
        .follow_links(false)
        .into_iter()
        .filter_map(|entry| match entry {
            Ok(entry) if entry.file_type().is_file() => Some(Ok(entry)),
            Ok(_) => None,
            Err(error) => Some(Err(error)),
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    paths.sort_by_key(|entry| entry.path().to_owned());

    for entry in paths {
        let full_path = entry.path();
        let relative = full_path
            .strip_prefix(&input)
            .context("walked file escaped input root")?;
        let portable = relative.to_string_lossy().replace('\\', "/");
        if portable.is_empty() {
            continue;
        }
        let metadata = fs::metadata(full_path)?;
        let file = File::open(full_path)?;
        let mut stream = StreamCDC::new(
            BufReader::new(file),
            options.chunking.minimum_bytes as usize,
            options.chunking.average_bytes as usize,
            options.chunking.maximum_bytes as usize,
        );
        let mut file_hasher = blake3::Hasher::new();
        let mut file_chunks = Vec::new();
        let mut file_size = 0_u64;
        while let Some(chunk) = stream.next() {
            let chunk = chunk
                .map_err(|error| anyhow::anyhow!("FastCDC failed for {portable}: {error:?}"))?;
            let raw_hash = blake3::hash(&chunk.data).to_hex().to_string();
            let encoded = zstd::stream::encode_all(&chunk.data[..], options.encoding.level)?;
            let encoded_hash = blake3::hash(&encoded).to_hex().to_string();
            let object_path = output
                .join("chunks/encoded")
                .join(format!("{encoded_hash}.bin"));
            if object_path.exists() {
                reused_chunks += 1;
            } else {
                let temporary = object_path.with_extension("bin.part");
                fs::write(&temporary, &encoded)?;
                fs::rename(temporary, &object_path)?;
                unique_chunks += 1;
            }
            file_hasher.update(&chunk.data);
            file_size += chunk.data.len() as u64;
            raw_bytes += chunk.data.len() as u64;
            encoded_bytes += encoded.len() as u64;
            chunks += 1;
            file_chunks.push(ChunkRef {
                raw_hash,
                raw_size: chunk.data.len() as u64,
                encoded_hash: encoded_hash.clone(),
                encoded_size: encoded.len() as u64,
                object_key: format!("chunks/encoded/{encoded_hash}.bin"),
            });
        }
        if file_size != metadata.len() {
            warnings.push(format!("file size changed during package: {portable}"));
        }
        files.push(FileRecipe {
            path: portable,
            size: file_size,
            blake3: file_hasher.finalize().to_hex().to_string(),
            chunks: file_chunks,
        });
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    if files.is_empty() {
        anyhow::bail!("input directory contains no regular files");
    }
    let executable = options
        .executable
        .clone()
        .or_else(|| {
            files
                .iter()
                .find(|file| file.path.to_ascii_lowercase().ends_with(".exe"))
                .map(|file| file.path.clone())
        })
        .unwrap_or_else(|| files[0].path.clone());
    let working_directory = Path::new(&executable)
        .parent()
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .filter(|path| !path.is_empty())
        .unwrap_or_else(|| ".".to_owned());
    let manifest = Manifest {
        schema_version: MANIFEST_SCHEMA_VERSION,
        manifest_id: Uuid::new_v4().to_string(),
        game_id: options.game_id.clone(),
        build_id: options.build_id.clone(),
        display_version: options.display_version.clone(),
        generated_at: Utc::now(),
        chunking: options.chunking.clone(),
        encoding: options.encoding.clone(),
        files,
        launch: LaunchProfile {
            executable,
            working_directory,
            arguments: Vec::new(),
            environment: BTreeMap::new(),
        },
    };
    manifest
        .validate()
        .map_err(|error| anyhow::anyhow!("manifest validation failed: {error}"))?;
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
    fs::write(output.join("manifest.json"), &manifest_bytes)?;
    let report = PackageReport {
        manifest_id: manifest.manifest_id,
        files: manifest.files.len() as u64,
        raw_bytes,
        encoded_bytes,
        chunks,
        unique_chunks,
        reused_chunks,
        warnings,
    };
    fs::write(
        output.join("report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    Ok(report)
}
