use anyhow::{Context, Result};
use chrono::Utc;
use fastcdc::v2020::StreamCDC;
use launcher_common::{
    ChunkRef, ChunkingConfig, EncodingConfig, FileRecipe, LaunchProfile, MANIFEST_SCHEMA_VERSION,
    Manifest,
};
use launcher_packs::{PackConfig, PackInput, build_packs};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufReader, Cursor, Read};
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
    pub failure_injection: Option<PackagingFailureInjection>,
    /// When set, emit immutable physical packs in addition to legacy logical
    /// chunk objects.  Keeping this optional makes the current Railway
    /// staging publication path backwards compatible.
    pub pack_config: Option<PackConfig>,
}

#[derive(Debug, Clone, Copy)]
pub struct PackagingFailureInjection {
    pub fail_after_chunks: u64,
    pub fail_after_manifest: bool,
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
            failure_injection: None,
            pack_config: None,
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
    pub packs: u64,
    pub packed_bytes: u64,
    pub warnings: Vec<String>,
}

pub fn validate_chunking_config(config: &ChunkingConfig) -> Result<()> {
    if config.algorithm != "fastcdc"
        || config.format_version != 1
        || config.minimum_bytes < 64
        || config.minimum_bytes > 1_048_576
        || config.average_bytes < 256
        || config.average_bytes > 4_194_304
        || config.maximum_bytes < 1_024
        || config.maximum_bytes > 16_777_216
        || config.minimum_bytes > config.average_bytes
        || config.average_bytes > config.maximum_bytes
    {
        anyhow::bail!(
            "FastCDC 4.0 parameters must be within 64..1 MiB, 256..4 MiB, and 1 KiB..16 MiB"
        );
    }
    Ok(())
}

/// Test/benchmark helper that exposes the exact v2020 stream boundaries used by
/// the packager. Production packaging still uses the file reader directly so
/// an entire file is never accumulated in memory.
pub fn chunk_bytes(bytes: &[u8], config: &ChunkingConfig) -> Result<Vec<Vec<u8>>> {
    let mut reader = Cursor::new(bytes);
    let mut chunks = Vec::new();
    chunk_reader(&mut reader, config, |chunk| {
        chunks.push(chunk.to_vec());
        Ok(())
    })?;
    Ok(chunks)
}

pub fn chunk_reader<R: Read, F: FnMut(&[u8]) -> Result<()>>(
    reader: R,
    config: &ChunkingConfig,
    mut on_chunk: F,
) -> Result<()> {
    validate_chunking_config(config)?;
    let stream = StreamCDC::new(
        reader,
        config.minimum_bytes as usize,
        config.average_bytes as usize,
        config.maximum_bytes as usize,
    );
    for chunk in stream {
        let chunk = chunk.map_err(|error| anyhow::anyhow!("FastCDC failed: {error:?}"))?;
        on_chunk(&chunk.data)?;
    }
    Ok(())
}

pub fn package_directory(
    input: impl AsRef<Path>,
    output: impl AsRef<Path>,
    options: &PackageOptions,
) -> Result<PackageReport> {
    validate_chunking_config(&options.chunking)?;
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
        let stream = StreamCDC::new(
            BufReader::new(file),
            options.chunking.minimum_bytes as usize,
            options.chunking.average_bytes as usize,
            options.chunking.maximum_bytes as usize,
        );
        let mut file_hasher = blake3::Hasher::new();
        let mut file_chunks = Vec::new();
        let mut file_size = 0_u64;
        for chunk in stream {
            let chunk = chunk
                .map_err(|error| anyhow::anyhow!("FastCDC failed for {portable}: {error:?}"))?;
            if options
                .failure_injection
                .is_some_and(|injection| chunks >= injection.fail_after_chunks)
            {
                anyhow::bail!("deterministic packaging failure after {chunks} chunks");
            }
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

    let (packs, packed_bytes) = if let Some(pack_config) = &options.pack_config {
        let mut unique_inputs = BTreeMap::<String, PackInput>::new();
        for file in &manifest.files {
            for chunk in &file.chunks {
                if unique_inputs.contains_key(&chunk.encoded_hash) {
                    continue;
                }
                let chunk_path = output.join(&chunk.object_key);
                let encoded_bytes = fs::read(&chunk_path).with_context(|| {
                    format!("could not read {} for pack output", chunk_path.display())
                })?;
                unique_inputs.insert(
                    chunk.encoded_hash.clone(),
                    PackInput::new(
                        chunk.encoded_hash.clone(),
                        chunk.raw_hash.clone(),
                        chunk.raw_size,
                        encoded_bytes,
                    ),
                );
            }
        }
        let artifacts = build_packs(unique_inputs.into_values(), pack_config.clone())?;
        fs::create_dir_all(output.join("packs"))?;
        let mut index = Vec::with_capacity(artifacts.len());
        let mut total_bytes = 0_u64;
        for artifact in artifacts {
            let path = output
                .join("packs")
                .join(format!("{}.pack", artifact.pack_hash));
            let temporary = path.with_extension("pack.part");
            fs::write(&temporary, &artifact.bytes)?;
            fs::rename(temporary, path)?;
            total_bytes = total_bytes.saturating_add(artifact.bytes.len() as u64);
            index.push(serde_json::json!({
                "pack_hash": artifact.pack_hash,
                "encoded_size": artifact.bytes.len(),
                "chunk_hashes": artifact.entries.iter().map(|entry| entry.encoded_hash.clone()).collect::<Vec<_>>(),
            }));
        }
        fs::write(
            output.join("pack-index.json"),
            serde_json::to_vec_pretty(&index)?,
        )?;
        (index.len() as u64, total_bytes)
    } else {
        (0, 0)
    };
    fs::write(output.join("manifest.json"), &manifest_bytes)?;
    if options
        .failure_injection
        .is_some_and(|injection| injection.fail_after_manifest)
    {
        anyhow::bail!("deterministic packaging failure after manifest creation");
    }
    let report = PackageReport {
        manifest_id: manifest.manifest_id,
        files: manifest.files.len() as u64,
        raw_bytes,
        encoded_bytes,
        chunks,
        unique_chunks,
        reused_chunks,
        packs,
        packed_bytes,
        warnings,
    };
    fs::write(
        output.join("report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn test_config() -> ChunkingConfig {
        ChunkingConfig {
            minimum_bytes: 4 * 1024,
            average_bytes: 16 * 1024,
            maximum_bytes: 64 * 1024,
            ..ChunkingConfig::default()
        }
    }

    fn seeded_bytes(length: usize) -> Vec<u8> {
        let mut value = 0x1234_5678_u64;
        (0..length)
            .map(|_| {
                value ^= value << 13;
                value ^= value >> 7;
                value ^= value << 17;
                value as u8
            })
            .collect()
    }

    fn hashes(chunks: &[Vec<u8>]) -> Vec<String> {
        chunks
            .iter()
            .map(|chunk| blake3::hash(chunk).to_hex().to_string())
            .collect()
    }

    #[test]
    fn fastcdc_boundaries_are_deterministic_for_tiny_boundary_and_random_inputs() {
        let config = test_config();
        for bytes in [
            vec![7_u8; 3],
            vec![9_u8; 4 * 1024],
            vec![3_u8; 128 * 1024],
            seeded_bytes(2 * 1024 * 1024),
        ] {
            let first = chunk_bytes(&bytes, &config).unwrap();
            let second = chunk_bytes(&bytes, &config).unwrap();
            assert_eq!(hashes(&first), hashes(&second));
            assert_eq!(first.iter().map(Vec::len).sum::<usize>(), bytes.len());
            assert!(
                first
                    .iter()
                    .all(|chunk| chunk.len() <= config.maximum_bytes as usize)
            );
        }
    }

    #[test]
    fn insertion_eventually_resynchronizes_content_defined_boundaries() {
        let config = test_config();
        let original = seeded_bytes(4 * 1024 * 1024);
        let mut inserted = original.clone();
        inserted.splice(32 * 1024..32 * 1024, [0xA5; 137]);
        let original_hashes = hashes(&chunk_bytes(&original, &config).unwrap());
        let inserted_hashes = hashes(&chunk_bytes(&inserted, &config).unwrap());
        let common_after_insertion = inserted_hashes
            .iter()
            .skip(1)
            .filter(|hash| original_hashes.contains(hash))
            .count();
        assert!(
            common_after_insertion > 0,
            "FastCDC did not resynchronize any later chunk"
        );
    }

    #[test]
    fn blake3_streaming_and_one_shot_match_and_corruption_is_detected() {
        let bytes = seeded_bytes(512 * 1024);
        let one_shot = blake3::hash(&bytes).to_hex().to_string();
        let mut hasher = blake3::Hasher::new();
        for part in bytes.chunks(8191) {
            hasher.update(part);
        }
        assert_eq!(one_shot, hasher.finalize().to_hex().to_string());
        let mut corrupt = bytes.clone();
        corrupt[123] ^= 1;
        assert_ne!(one_shot, blake3::hash(&corrupt).to_hex().to_string());
    }

    #[test]
    fn zstd_round_trip_is_byte_exact_and_corruption_fails() {
        let bytes = seeded_bytes(256 * 1024);
        let encoded = zstd::stream::encode_all(&bytes[..], 3).unwrap();
        let decoded = zstd::stream::decode_all(&encoded[..]).unwrap();
        assert_eq!(decoded, bytes);
        let mut corrupt = encoded.clone();
        let corruption_index = corrupt.len() / 2;
        corrupt[corruption_index] ^= 0x80;
        if let Ok(decoded) = zstd::stream::decode_all(&corrupt[..]) {
            assert_ne!(decoded, bytes);
        }
    }

    #[test]
    fn package_is_sorted_deduplicated_and_manifest_consistent() {
        let root = std::env::temp_dir().join(format!("launcher-package-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let input = root.join("input");
        let output = root.join("output");
        fs::create_dir_all(input.join("nested")).unwrap();
        fs::write(input.join("z.bin"), vec![4_u8; 100_000]).unwrap();
        fs::write(input.join("nested/a.bin"), vec![4_u8; 100_000]).unwrap();
        let options = PackageOptions {
            game_id: "game".into(),
            build_id: "build".into(),
            display_version: "A".into(),
            chunking: test_config(),
            ..PackageOptions::default()
        };
        let report = package_directory(&input, &output, &options).unwrap();
        let manifest: Manifest =
            serde_json::from_slice(&fs::read(output.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(
            manifest
                .files
                .iter()
                .map(|file| file.path.as_str())
                .collect::<Vec<_>>(),
            vec!["nested/a.bin", "z.bin"]
        );
        assert_eq!(report.files, 2);
        assert!(report.unique_chunks < report.chunks);
        for file in &manifest.files {
            assert_eq!(
                file.size,
                file.chunks.iter().map(|chunk| chunk.raw_size).sum::<u64>()
            );
            for chunk in &file.chunks {
                let bytes = fs::read(output.join(&chunk.object_key)).unwrap();
                assert_eq!(bytes.len() as u64, chunk.encoded_size);
                assert_eq!(
                    blake3::hash(&bytes).to_hex().to_string(),
                    chunk.encoded_hash
                );
            }
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn physical_pack_output_is_opt_in_and_indexed_by_pack_hash() {
        let root =
            std::env::temp_dir().join(format!("launcher-package-packs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let input = root.join("input");
        let output = root.join("output");
        fs::create_dir_all(&input).unwrap();
        fs::write(input.join("game.exe"), seeded_bytes(128 * 1024)).unwrap();
        let report = package_directory(
            &input,
            &output,
            &PackageOptions {
                game_id: "game".into(),
                build_id: "build".into(),
                display_version: "A".into(),
                executable: Some("game.exe".into()),
                chunking: test_config(),
                pack_config: Some(launcher_packs::PackConfig {
                    target_bytes: 1024 * 1024,
                    min_bytes: 1,
                    max_bytes: 2 * 1024 * 1024,
                }),
                ..PackageOptions::default()
            },
        )
        .unwrap();
        assert_eq!(report.packs, 1);
        let pack_files = fs::read_dir(output.join("packs"))
            .unwrap()
            .collect::<Vec<_>>();
        assert_eq!(pack_files.len(), 1);
        let pack = pack_files[0].as_ref().unwrap().path();
        let hash = pack.file_stem().unwrap().to_string_lossy().to_string();
        let bytes = fs::read(&pack).unwrap();
        assert_eq!(blake3::hash(&bytes).to_hex().to_string(), hash);
        let reader = launcher_packs::PackReader::parse(&bytes).unwrap();
        reader.verify_pack_hash(&hash).unwrap();
        assert_eq!(reader.entries().len(), report.unique_chunks as usize);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn packaging_failure_injection_leaves_only_verified_objects_and_no_manifest() {
        let root =
            std::env::temp_dir().join(format!("launcher-package-failure-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let input = root.join("input");
        let output = root.join("output");
        fs::create_dir_all(&input).unwrap();
        fs::write(input.join("game.exe"), seeded_bytes(256 * 1024)).unwrap();
        let options = PackageOptions {
            game_id: "game".into(),
            build_id: "build".into(),
            display_version: "A".into(),
            executable: Some("game.exe".into()),
            chunking: test_config(),
            failure_injection: Some(PackagingFailureInjection {
                fail_after_chunks: 1,
                fail_after_manifest: false,
            }),
            ..PackageOptions::default()
        };
        assert!(package_directory(&input, &output, &options).is_err());
        assert!(!output.join("manifest.json").exists());
        assert!(
            walkdir::WalkDir::new(&output)
                .into_iter()
                .filter_map(Result::ok)
                .all(|entry| {
                    entry
                        .path()
                        .extension()
                        .is_none_or(|extension| extension != "part")
                })
        );
        for entry in walkdir::WalkDir::new(output.join("chunks/encoded"))
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
        {
            let bytes = fs::read(entry.path()).unwrap();
            assert_eq!(
                blake3::hash(&bytes).to_hex().to_string(),
                entry.file_name().to_string_lossy().trim_end_matches(".bin")
            );
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn packaging_failure_after_manifest_creation_does_not_emit_ready_report() {
        let root = std::env::temp_dir().join(format!(
            "launcher-package-manifest-failure-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let input = root.join("input");
        let output = root.join("output");
        fs::create_dir_all(&input).unwrap();
        fs::write(input.join("game.exe"), b"game").unwrap();
        let options = PackageOptions {
            game_id: "game".into(),
            build_id: "build".into(),
            display_version: "A".into(),
            executable: Some("game.exe".into()),
            chunking: test_config(),
            failure_injection: Some(PackagingFailureInjection {
                fail_after_chunks: u64::MAX,
                fail_after_manifest: true,
            }),
            ..PackageOptions::default()
        };
        assert!(package_directory(&input, &output, &options).is_err());
        assert!(output.join("manifest.json").exists());
        assert!(!output.join("report.json").exists());
        let _ = fs::remove_dir_all(root);
    }
}
