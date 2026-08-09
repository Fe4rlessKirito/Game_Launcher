use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const MANIFEST_SCHEMA_VERSION: u32 = 1;

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
            minimum_bytes: 1 * 1024 * 1024,
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

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ManifestValidationError {
    #[error("unsupported manifest schema version {0}")]
    SchemaVersion(u32),
    #[error("invalid chunking configuration: {0}")]
    Chunking(String),
    #[error("invalid encoding configuration")]
    Encoding,
    #[error("invalid manifest path: {0}")]
    Path(String),
    #[error("duplicate manifest path: {0}")]
    DuplicatePath(String),
    #[error("invalid hash for {field}: {value}")]
    Hash { field: &'static str, value: String },
    #[error("file size does not equal sum of chunks for {path}")]
    FileSize { path: String },
    #[error("launch executable must be an owned file path")]
    LaunchExecutable,
}

impl Manifest {
    pub fn validate(&self) -> Result<(), ManifestValidationError> {
        if self.schema_version != MANIFEST_SCHEMA_VERSION {
            return Err(ManifestValidationError::SchemaVersion(self.schema_version));
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
        for file in &self.files {
            let normalized = normalize_manifest_path(&file.path)?;
            if normalized != file.path {
                return Err(ManifestValidationError::Path(file.path.clone()));
            }
            if !paths.insert(file.path.clone()) {
                return Err(ManifestValidationError::DuplicatePath(file.path.clone()));
            }
            validate_hash("file", &file.blake3)?;
            let chunk_size: u64 = file.chunks.iter().map(|chunk| chunk.raw_size).sum();
            if chunk_size != file.size {
                return Err(ManifestValidationError::FileSize {
                    path: file.path.clone(),
                });
            }
            for chunk in &file.chunks {
                validate_hash("raw", &chunk.raw_hash)?;
                validate_hash("encoded", &chunk.encoded_hash)?;
                let expected_key = format!("chunks/encoded/{}.bin", chunk.encoded_hash);
                if chunk.object_key != expected_key {
                    return Err(ManifestValidationError::Path(chunk.object_key.clone()));
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
        Ok(())
    }
}

pub fn normalize_manifest_path(path: &str) -> Result<String, ManifestValidationError> {
    if path.is_empty() || path.contains('\\') || path.starts_with('/') || path.contains(':') {
        return Err(ManifestValidationError::Path(path.to_owned()));
    }
    let mut parts = Vec::new();
    for part in path.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return Err(ManifestValidationError::Path(path.to_owned()));
        }
        parts.push(part);
    }
    Ok(parts.join("/"))
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
}
