use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use unicode_normalization::UnicodeNormalization;

pub mod work_status;

pub const MANIFEST_SCHEMA_VERSION: u32 = 1;
pub const MANIFEST_SIGNATURE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChunkingConfig {
    pub algorithm: String,
    pub format_version: u32,
    pub minimum_bytes: u64,
    pub average_bytes: u64,
    pub maximum_bytes: u64,
}

impl Default for ChunkingConfig {
    fn default() -> Self {
        Self {
            algorithm: "fastcdc".to_owned(),
            format_version: 1,
            minimum_bytes: 1024 * 1024,
            average_bytes: 4 * 1024 * 1024,
            maximum_bytes: 16 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EncodingConfig {
    pub id: String,
    pub level: i32,
}

impl Default for EncodingConfig {
    fn default() -> Self {
        Self {
            id: "zstd-v1-level-3".to_owned(),
            level: 3,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChunkRef {
    pub raw_hash: String,
    pub raw_size: u64,
    pub encoded_hash: String,
    pub encoded_size: u64,
    pub object_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileRecipe {
    pub path: String,
    pub size: u64,
    pub blake3: String,
    pub chunks: Vec<ChunkRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct LaunchProfile {
    pub executable: String,
    pub working_directory: String,
    #[serde(default)]
    pub arguments: Vec<String>,
    #[serde(default)]
    pub environment: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    pub schema_version: u32,
    pub manifest_id: String,
    pub game_id: String,
    pub build_id: String,
    pub display_version: String,
    pub generated_at: DateTime<Utc>,
    pub chunking: ChunkingConfig,
    pub encoding: EncodingConfig,
    pub files: Vec<FileRecipe>,
    pub launch: LaunchProfile,
}

/// A detached signature over the exact UTF-8 bytes served as `manifest.json`.
/// The embedded public key is permitted only for local fixtures; production
/// clients must resolve `key_id` through a trusted key ring.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManifestSignature {
    pub schema_version: u32,
    pub algorithm: String,
    pub key_id: String,
    pub manifest_blake3: String,
    pub signature_base64: String,
    #[serde(default)]
    pub public_key_base64: Option<String>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ManifestValidationError {
    #[error("unsupported manifest schema version {0}")]
    SchemaVersion(u32),
    #[error("invalid chunking configuration: {0}")]
    Chunking(String),
    #[error("invalid encoding configuration")]
    Encoding,
    #[error("manifest identity fields are required")]
    Identity,
    #[error("invalid manifest path: {0}")]
    Path(String),
    #[error("duplicate manifest path: {0}")]
    DuplicatePath(String),
    #[error("invalid hash for {field}: {value}")]
    Hash { field: &'static str, value: String },
    #[error("file size does not equal sum of chunks for {path}")]
    FileSize { path: String },
    #[error("invalid chunk size in {path}")]
    ChunkSize { path: String },
    #[error("conflicting duplicate chunk metadata: {hash}")]
    ConflictingChunk { hash: String },
    #[error("launch executable must be an owned file path")]
    LaunchExecutable,
    #[error("launch working directory is invalid")]
    LaunchWorkingDirectory,
}

impl Manifest {
    pub fn validate(&self) -> Result<(), ManifestValidationError> {
        if self.schema_version != MANIFEST_SCHEMA_VERSION {
            return Err(ManifestValidationError::SchemaVersion(self.schema_version));
        }
        if self.manifest_id.trim().is_empty()
            || self.game_id.trim().is_empty()
            || self.build_id.trim().is_empty()
            || self.display_version.trim().is_empty()
        {
            return Err(ManifestValidationError::Identity);
        }
        let c = &self.chunking;
        if c.algorithm != "fastcdc"
            || c.format_version != 1
            || c.minimum_bytes == 0
            || c.minimum_bytes > c.average_bytes
            || c.average_bytes > c.maximum_bytes
        {
            return Err(ManifestValidationError::Chunking(format!(
                "{}:{}:{}:{}",
                c.algorithm, c.minimum_bytes, c.average_bytes, c.maximum_bytes
            )));
        }
        if self.encoding.id != "zstd-v1-level-3" || self.encoding.level != 3 {
            return Err(ManifestValidationError::Encoding);
        }

        let mut paths = std::collections::BTreeSet::new();
        let mut chunks_by_encoded_hash = std::collections::BTreeMap::new();
        for file in &self.files {
            let normalized = normalize_manifest_path(&file.path)?;
            if normalized != file.path {
                return Err(ManifestValidationError::Path(file.path.clone()));
            }
            if !paths.insert(file.path.clone()) {
                return Err(ManifestValidationError::DuplicatePath(file.path.clone()));
            }
            validate_hash("file", &file.blake3)?;
            let chunk_size = file
                .chunks
                .iter()
                .try_fold(0_u64, |total, chunk| total.checked_add(chunk.raw_size));
            let Some(chunk_size) = chunk_size else {
                return Err(ManifestValidationError::FileSize {
                    path: file.path.clone(),
                });
            };
            if chunk_size != file.size {
                return Err(ManifestValidationError::FileSize {
                    path: file.path.clone(),
                });
            }
            for chunk in &file.chunks {
                if chunk.raw_size == 0 || chunk.encoded_size == 0 {
                    return Err(ManifestValidationError::ChunkSize {
                        path: file.path.clone(),
                    });
                }
                validate_hash("raw", &chunk.raw_hash)?;
                validate_hash("encoded", &chunk.encoded_hash)?;
                let expected_key = format!("chunks/encoded/{}.bin", chunk.encoded_hash);
                if chunk.object_key != expected_key {
                    return Err(ManifestValidationError::Path(chunk.object_key.clone()));
                }
                if let Some(existing) = chunks_by_encoded_hash.get(&chunk.encoded_hash) {
                    if *existing != chunk {
                        return Err(ManifestValidationError::ConflictingChunk {
                            hash: chunk.encoded_hash.clone(),
                        });
                    }
                } else {
                    chunks_by_encoded_hash.insert(chunk.encoded_hash.clone(), chunk);
                }
            }
        }
        let executable = normalize_manifest_path(&self.launch.executable)
            .map_err(|_| ManifestValidationError::LaunchExecutable)?;
        if executable != self.launch.executable {
            return Err(ManifestValidationError::LaunchExecutable);
        }
        if !self.files.iter().any(|file| file.path == executable) {
            return Err(ManifestValidationError::LaunchExecutable);
        }
        if self.launch.working_directory != "." {
            let working_directory = normalize_manifest_path(&self.launch.working_directory)
                .map_err(|_| ManifestValidationError::LaunchWorkingDirectory)?;
            if working_directory != self.launch.working_directory {
                return Err(ManifestValidationError::LaunchWorkingDirectory);
            }
        }
        Ok(())
    }
}

pub fn normalize_manifest_path(path: &str) -> Result<String, ManifestValidationError> {
    if path.is_empty() || path.contains('\\') || path.starts_with('/') || path.contains(':') {
        return Err(ManifestValidationError::Path(path.to_owned()));
    }
    if path.nfc().collect::<String>() != path {
        return Err(ManifestValidationError::Path(path.to_owned()));
    }
    let mut parts = Vec::new();
    for part in path.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return Err(ManifestValidationError::Path(path.to_owned()));
        }
        if part.chars().any(|character| character <= '\u{001f}')
            || part.ends_with(' ')
            || part.ends_with('.')
            || is_reserved_windows_name(part)
        {
            return Err(ManifestValidationError::Path(path.to_owned()));
        }
        parts.push(part);
    }
    Ok(parts.join("/"))
}

fn is_reserved_windows_name(part: &str) -> bool {
    let stem = part
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit()
            && stem.as_bytes()[3] >= b'1'
            && stem.as_bytes()[3] <= b'9')
}

fn validate_hash(field: &'static str, value: &str) -> Result<(), ManifestValidationError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(ManifestValidationError::Hash {
            field,
            value: value.to_owned(),
        });
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GameSummary {
    pub id: String,
    pub slug: String,
    pub title: String,
    pub description: String,
    pub hero_image_url: Option<String>,
    pub cover_image_url: Option<String>,
    pub latest_build: Option<BuildSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BuildSummary {
    pub id: String,
    pub game_id: String,
    pub display_version: String,
    pub size_bytes: u64,
    pub published_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CatalogPage {
    pub items: Vec<GameSummary>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChunkResolutionRequest {
    pub encoded_hashes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedChunk {
    pub encoded_hash: String,
    pub urls: Vec<String>,
    pub expires_at: Option<DateTime<Utc>>,
}

/// A source that the launcher may use for a physical HOT pack.  COLD
/// providers are deliberately not represented by this type and must never be
/// returned to an end user.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HotPackSource {
    pub provider: String,
    pub pool_id: String,
    pub provider_type: String,
    pub failure_domain: String,
    pub url: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub range_supported: bool,
    pub stable_url: bool,
    pub priority: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PackResolutionRequest {
    pub encoded_hashes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedPack {
    pub pack_hash: String,
    pub encoded_size: u64,
    pub chunk_hashes: Vec<String>,
    pub sources: Vec<HotPackSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApiErrorBody {
    pub code: String,
    pub message: String,
    pub request_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_traversal_and_absolute_paths() {
        for path in [
            "../escape.txt",
            "/absolute.txt",
            "C:/windows.txt",
            "a\\b.txt",
            "a/../b",
            "CON/file.bin",
            "folder/trailing. ",
            "folder/control\u{001f}.bin",
        ] {
            assert!(normalize_manifest_path(path).is_err(), "{path}");
        }
    }

    #[test]
    fn accepts_portable_nested_path() {
        assert_eq!(
            normalize_manifest_path("Game/Binaries/Game.exe").unwrap(),
            "Game/Binaries/Game.exe"
        );
    }

    #[test]
    fn rejects_invalid_chunk_metadata_and_working_directory() {
        let raw_hash = "a".repeat(64);
        let encoded_hash = "b".repeat(64);
        let chunk = ChunkRef {
            raw_hash: raw_hash.clone(),
            raw_size: 1,
            encoded_hash: encoded_hash.clone(),
            encoded_size: 1,
            object_key: format!("chunks/encoded/{encoded_hash}.bin"),
        };
        let manifest = || Manifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            manifest_id: "manifest".to_owned(),
            game_id: "game".to_owned(),
            build_id: "build".to_owned(),
            display_version: "A".to_owned(),
            generated_at: Utc::now(),
            chunking: ChunkingConfig::default(),
            encoding: EncodingConfig::default(),
            files: vec![FileRecipe {
                path: "game.exe".to_owned(),
                size: 1,
                blake3: raw_hash.clone(),
                chunks: vec![chunk.clone()],
            }],
            launch: LaunchProfile {
                executable: "game.exe".to_owned(),
                working_directory: ".".to_owned(),
                arguments: vec![],
                environment: std::collections::BTreeMap::new(),
            },
        };

        let mut zero_sized = manifest();
        zero_sized.files[0].chunks[0].raw_size = 0;
        zero_sized.files[0].size = 0;
        assert!(zero_sized.validate().is_err());

        let mut conflicting = manifest();
        conflicting.files[0].size = 2;
        conflicting.files[0].chunks.push(ChunkRef {
            raw_hash: "c".repeat(64),
            raw_size: 1,
            encoded_hash: chunk.encoded_hash.clone(),
            encoded_size: chunk.encoded_size,
            object_key: chunk.object_key.clone(),
        });
        assert!(conflicting.validate().is_err());

        let mut invalid_working_directory = manifest();
        invalid_working_directory.launch.working_directory = "game/./data".to_owned();
        assert!(invalid_working_directory.validate().is_err());
    }
}
