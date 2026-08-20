//! Immutable physical pack files for the launcher data plane.
//!
//! A pack contains already encoded logical chunks.  The pack identity is the
//! BLAKE3 digest of the complete immutable byte sequence and is intentionally
//! not stored inside the sequence itself.  That keeps the identity stable and
//! makes the format safe to copy between providers.

use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use thiserror::Error;

const HEADER_MAGIC: &[u8; 8] = b"LGRPACK1";
const FOOTER_MAGIC: &[u8; 8] = b"LGRPFTR1";
const FORMAT_VERSION: u16 = 1;
const HEADER_SIZE: usize = 64;
const ENTRY_SIZE: usize = 96;
const FOOTER_SIZE: usize = 72;
const COMPRESSION_ZSTD: u32 = 1;
const DEFAULT_TARGET_BYTES: u64 = 512 * 1024 * 1024;
const DEFAULT_MIN_BYTES: u64 = 256 * 1024 * 1024;
const DEFAULT_MAX_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_PACK_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_DECLARED_CHUNK_BYTES: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum PackError {
    #[error("pack file I/O failed: {0}")]
    Io(String),
    #[error("pack configuration is invalid: {0}")]
    Configuration(String),
    #[error("pack is truncated or too small")]
    Truncated,
    #[error("pack magic is invalid")]
    InvalidMagic,
    #[error("unsupported pack format version {0}")]
    UnsupportedVersion(u16),
    #[error("pack header is invalid: {0}")]
    InvalidHeader(String),
    #[error("pack footer is invalid: {0}")]
    InvalidFooter(String),
    #[error("pack index is invalid: {0}")]
    InvalidIndex(String),
    #[error("pack identity mismatch: expected {expected}, got {actual}")]
    PackHashMismatch { expected: String, actual: String },
    #[error("chunk hash mismatch: expected {expected}, got {actual}")]
    ChunkHashMismatch { expected: String, actual: String },
    #[error("raw chunk hash mismatch: expected {expected}, got {actual}")]
    RawHashMismatch { expected: String, actual: String },
    #[error("chunk is not present in pack: {0}")]
    ChunkNotFound(String),
    #[error("chunk size mismatch")]
    ChunkSizeMismatch,
    #[error("unsupported pack compression {0}")]
    UnsupportedCompression(u32),
    #[error("chunk decompression failed: {0}")]
    Decompression(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PackConfig {
    pub target_bytes: u64,
    pub min_bytes: u64,
    pub max_bytes: u64,
}

impl Default for PackConfig {
    fn default() -> Self {
        Self {
            target_bytes: DEFAULT_TARGET_BYTES,
            min_bytes: DEFAULT_MIN_BYTES,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }
}

impl PackConfig {
    pub fn validate(&self) -> Result<(), PackError> {
        if self.min_bytes == 0
            || self.target_bytes < self.min_bytes
            || self.max_bytes < self.target_bytes
            || self.max_bytes > MAX_PACK_BYTES
        {
            return Err(PackError::Configuration(format!(
                "expected 0 < min <= target <= max <= {MAX_PACK_BYTES}, got min={} target={} max={}",
                self.min_bytes, self.target_bytes, self.max_bytes
            )));
        }
        Ok(())
    }

    pub fn from_env() -> Result<Self, PackError> {
        fn parse(name: &str, default: u64) -> Result<u64, PackError> {
            std::env::var(name)
                .unwrap_or_else(|_| default.to_string())
                .parse()
                .map_err(|error| {
                    PackError::Configuration(format!("{name} must be an integer: {error}"))
                })
        }
        let config = Self {
            target_bytes: parse("LAUNCHER_PACK_TARGET_BYTES", DEFAULT_TARGET_BYTES)?,
            min_bytes: parse("LAUNCHER_PACK_MIN_BYTES", DEFAULT_MIN_BYTES)?,
            max_bytes: parse("LAUNCHER_PACK_MAX_BYTES", DEFAULT_MAX_BYTES)?,
        };
        config.validate()?;
        Ok(config)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackInput {
    pub encoded_hash: String,
    pub raw_hash: String,
    pub raw_size: u64,
    pub encoded_bytes: Vec<u8>,
}

/// Metadata for an encoded chunk that is already present on disk.
///
/// This is intentionally separate from [`PackInput`].  `PackInput` is useful
/// for small in-memory callers and tests, while the production packager uses
/// this file-backed form so a multi-gigabyte build never has to be loaded into
/// RAM before its physical packs are written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackFileInput {
    pub encoded_hash: String,
    pub raw_hash: String,
    pub raw_size: u64,
    pub encoded_size: u64,
    pub encoded_path: PathBuf,
}

impl PackFileInput {
    pub fn new(
        encoded_hash: impl Into<String>,
        raw_hash: impl Into<String>,
        raw_size: u64,
        encoded_size: u64,
        encoded_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            encoded_hash: encoded_hash.into(),
            raw_hash: raw_hash.into(),
            raw_size,
            encoded_size,
            encoded_path: encoded_path.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackFileArtifact {
    pub pack_hash: String,
    pub encoded_size: u64,
    pub entries: Vec<PackEntry>,
}

impl PackInput {
    pub fn new(
        encoded_hash: impl Into<String>,
        raw_hash: impl Into<String>,
        raw_size: u64,
        encoded_bytes: Vec<u8>,
    ) -> Self {
        Self {
            encoded_hash: encoded_hash.into(),
            raw_hash: raw_hash.into(),
            raw_size,
            encoded_bytes,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackArtifact {
    pub pack_hash: String,
    pub bytes: Vec<u8>,
    pub entries: Vec<PackEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackEntry {
    pub encoded_hash: String,
    pub raw_hash: String,
    pub offset: u64,
    pub encoded_length: u64,
    pub raw_length: u64,
    pub compression: u32,
    pub flags: u32,
}

impl PackEntry {
    fn encoded_end(&self) -> Result<u64, PackError> {
        self.offset
            .checked_add(self.encoded_length)
            .ok_or_else(|| PackError::InvalidIndex("chunk offset overflows".to_owned()))
    }
}

#[derive(Debug, Clone)]
pub struct PackBuilder {
    config: PackConfig,
    inputs: Vec<PackInput>,
}

impl PackBuilder {
    pub fn new(config: PackConfig) -> Result<Self, PackError> {
        config.validate()?;
        Ok(Self {
            config,
            inputs: Vec::new(),
        })
    }

    pub fn push(&mut self, input: PackInput) -> Result<(), PackError> {
        validate_hash("encoded", &input.encoded_hash)?;
        validate_hash("raw", &input.raw_hash)?;
        if input.raw_size > MAX_DECLARED_CHUNK_BYTES
            || input.encoded_bytes.len() as u64 > MAX_DECLARED_CHUNK_BYTES
        {
            return Err(PackError::Configuration(
                "chunk exceeds the maximum declared size".to_owned(),
            ));
        }
        let actual = blake3::hash(&input.encoded_bytes).to_hex().to_string();
        if actual != input.encoded_hash {
            return Err(PackError::ChunkHashMismatch {
                expected: input.encoded_hash,
                actual,
            });
        }
        if self
            .inputs
            .iter()
            .any(|item| item.encoded_hash == input.encoded_hash)
        {
            return Err(PackError::InvalidIndex(format!(
                "duplicate encoded hash {}",
                input.encoded_hash
            )));
        }
        if input.encoded_bytes.len() as u64 > self.config.max_bytes {
            return Err(PackError::Configuration(format!(
                "chunk {} is larger than the maximum pack size",
                input.encoded_hash
            )));
        }
        self.inputs.push(input);
        Ok(())
    }

    pub fn build(mut self) -> Result<Vec<PackArtifact>, PackError> {
        self.inputs
            .sort_by(|left, right| left.encoded_hash.cmp(&right.encoded_hash));
        let mut groups = Vec::<Vec<PackInput>>::new();
        let mut current = Vec::new();
        let mut current_bytes = 0u64;
        for input in self.inputs {
            let input_bytes = input.encoded_bytes.len() as u64;
            let should_flush = !current.is_empty()
                && (current_bytes.saturating_add(input_bytes) > self.config.max_bytes
                    || (current_bytes >= self.config.min_bytes
                        && current_bytes.saturating_add(input_bytes) > self.config.target_bytes));
            if should_flush {
                groups.push(std::mem::take(&mut current));
                current_bytes = 0;
            }
            current_bytes = current_bytes.saturating_add(input_bytes);
            current.push(input);
        }
        if !current.is_empty() {
            groups.push(current);
        }
        groups.into_iter().map(build_artifact).collect()
    }
}

pub fn build_packs<I>(inputs: I, config: PackConfig) -> Result<Vec<PackArtifact>, PackError>
where
    I: IntoIterator<Item = PackInput>,
{
    let mut builder = PackBuilder::new(config)?;
    for input in inputs {
        builder.push(input)?;
    }
    builder.build()
}

/// Write deterministic physical packs from encoded chunk files without
/// accumulating all chunk bytes in memory.
///
/// Inputs are sorted by encoded hash, matching [`build_packs`].  Each output
/// pack is streamed once: a small in-memory index is retained, while encoded
/// chunk data is copied directly from its source file to the pack.  The pack
/// hash is calculated over the exact bytes written and the temporary file is
/// renamed atomically after the footer has been written.
pub fn write_packs_from_files<I, P>(
    inputs: I,
    config: PackConfig,
    destination: P,
) -> Result<Vec<PackFileArtifact>, PackError>
where
    I: IntoIterator<Item = PackFileInput>,
    P: AsRef<Path>,
{
    config.validate()?;
    let mut inputs = inputs.into_iter().collect::<Vec<_>>();
    for input in &inputs {
        validate_hash("encoded", &input.encoded_hash)?;
        validate_hash("raw", &input.raw_hash)?;
        if input.raw_size > MAX_DECLARED_CHUNK_BYTES
            || input.encoded_size > MAX_DECLARED_CHUNK_BYTES
        {
            return Err(PackError::Configuration(
                "chunk exceeds the maximum declared size".to_owned(),
            ));
        }
        if input.encoded_size > config.max_bytes {
            return Err(PackError::Configuration(format!(
                "chunk {} is larger than the maximum pack size",
                input.encoded_hash
            )));
        }
        let metadata = fs::metadata(&input.encoded_path).map_err(io_error)?;
        if !metadata.is_file() {
            return Err(PackError::Io(format!(
                "encoded chunk path is not a file: {}",
                input.encoded_path.display()
            )));
        }
        if metadata.len() != input.encoded_size {
            return Err(PackError::InvalidIndex(format!(
                "encoded chunk {} has size {}, expected {}",
                input.encoded_hash,
                metadata.len(),
                input.encoded_size
            )));
        }
    }
    inputs.sort_by(|left, right| left.encoded_hash.cmp(&right.encoded_hash));
    for pair in inputs.windows(2) {
        if pair[0].encoded_hash == pair[1].encoded_hash {
            return Err(PackError::InvalidIndex(format!(
                "duplicate encoded hash {}",
                pair[0].encoded_hash
            )));
        }
    }

    let destination = destination.as_ref();
    fs::create_dir_all(destination).map_err(io_error)?;
    let mut groups = Vec::<Vec<PackFileInput>>::new();
    let mut current = Vec::new();
    let mut current_bytes = 0_u64;
    for input in inputs {
        let should_flush = !current.is_empty()
            && (current_bytes.saturating_add(input.encoded_size) > config.max_bytes
                || (current_bytes >= config.min_bytes
                    && current_bytes.saturating_add(input.encoded_size) > config.target_bytes));
        if should_flush {
            groups.push(std::mem::take(&mut current));
            current_bytes = 0;
        }
        current_bytes = current_bytes.saturating_add(input.encoded_size);
        current.push(input);
    }
    if !current.is_empty() {
        groups.push(current);
    }

    groups
        .into_iter()
        .enumerate()
        .map(|(ordinal, group)| write_file_pack(group, destination, ordinal))
        .collect()
}

fn write_file_pack(
    inputs: Vec<PackFileInput>,
    destination: &Path,
    ordinal: usize,
) -> Result<PackFileArtifact, PackError> {
    let data_length = inputs.iter().try_fold(0_u64, |total, input| {
        total
            .checked_add(input.encoded_size)
            .ok_or_else(|| PackError::InvalidHeader("data length overflows".to_owned()))
    })?;
    let index_offset = (HEADER_SIZE as u64)
        .checked_add(data_length)
        .ok_or_else(|| PackError::InvalidHeader("index offset overflows".to_owned()))?;
    let index_length = (inputs.len() as u64)
        .checked_mul(ENTRY_SIZE as u64)
        .ok_or_else(|| PackError::InvalidHeader("index length overflows".to_owned()))?;
    let total_length = index_offset
        .checked_add(index_length)
        .and_then(|length| length.checked_add(FOOTER_SIZE as u64))
        .ok_or_else(|| PackError::InvalidHeader("pack length overflows".to_owned()))?;
    if total_length > MAX_PACK_BYTES {
        return Err(PackError::Configuration(
            "pack exceeds the format size limit".to_owned(),
        ));
    }

    let temporary = destination.join(format!(".pack-{ordinal}.pack.part"));
    let result = (|| {
        let mut output = File::create(&temporary).map_err(io_error)?;
        let mut pack_hasher = blake3::Hasher::new();
        let mut header = [0_u8; HEADER_SIZE];
        header[0..8].copy_from_slice(HEADER_MAGIC);
        header[8..10].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
        header[12..16].copy_from_slice(&(HEADER_SIZE as u32).to_le_bytes());
        header[16..24].copy_from_slice(&(inputs.len() as u64).to_le_bytes());
        header[24..32].copy_from_slice(&(HEADER_SIZE as u64).to_le_bytes());
        header[32..40].copy_from_slice(&index_offset.to_le_bytes());
        header[40..48].copy_from_slice(&index_length.to_le_bytes());
        write_hashed(&mut output, &mut pack_hasher, &header)?;

        let mut entries = Vec::with_capacity(inputs.len());
        let mut offset = HEADER_SIZE as u64;
        let mut buffer = vec![0_u8; 1024 * 1024];
        for input in &inputs {
            let mut source = BufReader::new(File::open(&input.encoded_path).map_err(io_error)?);
            let mut chunk_hasher = blake3::Hasher::new();
            let mut copied = 0_u64;
            loop {
                let read = source.read(&mut buffer).map_err(io_error)?;
                if read == 0 {
                    break;
                }
                chunk_hasher.update(&buffer[..read]);
                write_hashed(&mut output, &mut pack_hasher, &buffer[..read])?;
                copied = copied
                    .checked_add(read as u64)
                    .ok_or_else(|| PackError::InvalidHeader("chunk size overflows".to_owned()))?;
            }
            if copied != input.encoded_size {
                return Err(PackError::InvalidIndex(format!(
                    "encoded chunk {} changed size while packing",
                    input.encoded_hash
                )));
            }
            let actual = chunk_hasher.finalize().to_hex().to_string();
            if actual != input.encoded_hash {
                return Err(PackError::ChunkHashMismatch {
                    expected: input.encoded_hash.clone(),
                    actual,
                });
            }
            entries.push(PackEntry {
                encoded_hash: input.encoded_hash.clone(),
                raw_hash: input.raw_hash.clone(),
                offset,
                encoded_length: input.encoded_size,
                raw_length: input.raw_size,
                compression: COMPRESSION_ZSTD,
                flags: 0,
            });
            offset = offset
                .checked_add(input.encoded_size)
                .ok_or_else(|| PackError::InvalidHeader("chunk offset overflows".to_owned()))?;
        }

        let mut index = Vec::with_capacity(index_length as usize);
        for entry in &entries {
            write_hash(&mut index, &entry.encoded_hash)?;
            write_hash(&mut index, &entry.raw_hash)?;
            index.extend_from_slice(&entry.offset.to_le_bytes());
            index.extend_from_slice(&entry.encoded_length.to_le_bytes());
            index.extend_from_slice(&entry.raw_length.to_le_bytes());
            index.extend_from_slice(&entry.compression.to_le_bytes());
            index.extend_from_slice(&entry.flags.to_le_bytes());
        }
        let index_hash = blake3::hash(&index);
        write_hashed(&mut output, &mut pack_hasher, &index)?;

        let mut footer = [0_u8; FOOTER_SIZE];
        footer[0..8].copy_from_slice(FOOTER_MAGIC);
        footer[8..10].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
        footer[12..20].copy_from_slice(&index_offset.to_le_bytes());
        footer[20..28].copy_from_slice(&index_length.to_le_bytes());
        footer[28..36].copy_from_slice(&(entries.len() as u64).to_le_bytes());
        footer[40..72].copy_from_slice(index_hash.as_bytes());
        write_hashed(&mut output, &mut pack_hasher, &footer)?;
        output.flush().map_err(io_error)?;

        let pack_hash = pack_hasher.finalize().to_hex().to_string();
        let final_path = destination.join(format!("{pack_hash}.pack"));
        fs::rename(&temporary, &final_path).map_err(io_error)?;
        Ok(PackFileArtifact {
            pack_hash,
            encoded_size: total_length,
            entries,
        })
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn write_hashed(
    output: &mut File,
    hasher: &mut blake3::Hasher,
    bytes: &[u8],
) -> Result<(), PackError> {
    output.write_all(bytes).map_err(io_error)?;
    hasher.update(bytes);
    Ok(())
}

fn io_error(error: std::io::Error) -> PackError {
    PackError::Io(error.to_string())
}

fn build_artifact(inputs: Vec<PackInput>) -> Result<PackArtifact, PackError> {
    let data_length = inputs.iter().try_fold(0u64, |total, input| {
        total
            .checked_add(input.encoded_bytes.len() as u64)
            .ok_or_else(|| PackError::InvalidHeader("data length overflows".to_owned()))
    })?;
    let index_offset = (HEADER_SIZE as u64)
        .checked_add(data_length)
        .ok_or_else(|| PackError::InvalidHeader("index offset overflows".to_owned()))?;
    let index_length = (inputs.len() as u64)
        .checked_mul(ENTRY_SIZE as u64)
        .ok_or_else(|| PackError::InvalidHeader("index length overflows".to_owned()))?;
    let total_length = index_offset
        .checked_add(index_length)
        .and_then(|length| length.checked_add(FOOTER_SIZE as u64))
        .ok_or_else(|| PackError::InvalidHeader("pack length overflows".to_owned()))?;
    if total_length > MAX_PACK_BYTES {
        return Err(PackError::Configuration(
            "pack exceeds the format size limit".to_owned(),
        ));
    }

    let mut bytes = vec![0u8; HEADER_SIZE];
    let mut entries = Vec::with_capacity(inputs.len());
    let mut offset = HEADER_SIZE as u64;
    for input in inputs {
        let encoded_length = input.encoded_bytes.len() as u64;
        bytes.extend_from_slice(&input.encoded_bytes);
        entries.push(PackEntry {
            encoded_hash: input.encoded_hash,
            raw_hash: input.raw_hash,
            offset,
            encoded_length,
            raw_length: input.raw_size,
            compression: COMPRESSION_ZSTD,
            flags: 0,
        });
        offset = offset
            .checked_add(encoded_length)
            .ok_or_else(|| PackError::InvalidHeader("chunk offset overflows".to_owned()))?;
    }

    let index_start = bytes.len();
    for entry in &entries {
        write_hash(&mut bytes, &entry.encoded_hash)?;
        write_hash(&mut bytes, &entry.raw_hash)?;
        bytes.extend_from_slice(&entry.offset.to_le_bytes());
        bytes.extend_from_slice(&entry.encoded_length.to_le_bytes());
        bytes.extend_from_slice(&entry.raw_length.to_le_bytes());
        bytes.extend_from_slice(&entry.compression.to_le_bytes());
        bytes.extend_from_slice(&entry.flags.to_le_bytes());
    }
    debug_assert_eq!(bytes.len() - index_start, index_length as usize);
    let index_hash = blake3::hash(&bytes[index_start..]);

    bytes.extend_from_slice(FOOTER_MAGIC);
    bytes.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    bytes.extend_from_slice(&0u16.to_le_bytes());
    bytes.extend_from_slice(&index_offset.to_le_bytes());
    bytes.extend_from_slice(&index_length.to_le_bytes());
    bytes.extend_from_slice(&(entries.len() as u64).to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(index_hash.as_bytes());

    bytes[0..8].copy_from_slice(HEADER_MAGIC);
    bytes[8..10].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
    bytes[10..12].copy_from_slice(&0u16.to_le_bytes());
    bytes[12..16].copy_from_slice(&(HEADER_SIZE as u32).to_le_bytes());
    bytes[16..24].copy_from_slice(&(entries.len() as u64).to_le_bytes());
    bytes[24..32].copy_from_slice(&(HEADER_SIZE as u64).to_le_bytes());
    bytes[32..40].copy_from_slice(&index_offset.to_le_bytes());
    bytes[40..48].copy_from_slice(&index_length.to_le_bytes());

    let pack_hash = blake3::hash(&bytes).to_hex().to_string();
    Ok(PackArtifact {
        pack_hash,
        bytes,
        entries,
    })
}

#[derive(Debug, Clone)]
pub struct PackReader<'a> {
    bytes: &'a [u8],
    entries: Vec<PackEntry>,
    index_offset: u64,
}

impl<'a> PackReader<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, PackError> {
        if bytes.len() < HEADER_SIZE + FOOTER_SIZE {
            return Err(PackError::Truncated);
        }
        if &bytes[..8] != HEADER_MAGIC {
            return Err(PackError::InvalidMagic);
        }
        let version = read_u16(bytes, 8).ok_or(PackError::Truncated)?;
        if version != FORMAT_VERSION {
            return Err(PackError::UnsupportedVersion(version));
        }
        let header_length = read_u32(bytes, 12).ok_or(PackError::Truncated)? as usize;
        if header_length != HEADER_SIZE {
            return Err(PackError::InvalidHeader(format!(
                "header length is {header_length}, expected {HEADER_SIZE}"
            )));
        }
        let entry_count = read_u64(bytes, 16).ok_or(PackError::Truncated)?;
        let data_offset = read_u64(bytes, 24).ok_or(PackError::Truncated)?;
        let index_offset = read_u64(bytes, 32).ok_or(PackError::Truncated)?;
        let index_length = read_u64(bytes, 40).ok_or(PackError::Truncated)?;
        if data_offset != HEADER_SIZE as u64 {
            return Err(PackError::InvalidHeader(
                "data offset is not after the header".to_owned(),
            ));
        }
        let expected_index_length =
            entry_count.checked_mul(ENTRY_SIZE as u64).ok_or_else(|| {
                PackError::InvalidHeader("entry count overflows index length".to_owned())
            })?;
        if index_length != expected_index_length {
            return Err(PackError::InvalidHeader(
                "index length does not match entry count".to_owned(),
            ));
        }
        if bytes.len() as u64 > MAX_PACK_BYTES {
            return Err(PackError::InvalidHeader(
                "pack exceeds the format size limit".to_owned(),
            ));
        }

        let footer_start = bytes.len() - FOOTER_SIZE;
        if &bytes[footer_start..footer_start + 8] != FOOTER_MAGIC {
            return Err(PackError::InvalidMagic);
        }
        let footer_version = read_u16(bytes, footer_start + 8).ok_or(PackError::Truncated)?;
        if footer_version != FORMAT_VERSION {
            return Err(PackError::UnsupportedVersion(footer_version));
        }
        let footer_index_offset = read_u64(bytes, footer_start + 12).ok_or(PackError::Truncated)?;
        let footer_index_length = read_u64(bytes, footer_start + 20).ok_or(PackError::Truncated)?;
        let footer_entry_count = read_u64(bytes, footer_start + 28).ok_or(PackError::Truncated)?;
        if footer_index_offset != index_offset
            || footer_index_length != index_length
            || footer_entry_count != entry_count
        {
            return Err(PackError::InvalidFooter(
                "footer does not match the header".to_owned(),
            ));
        }
        let index_end = index_offset
            .checked_add(index_length)
            .ok_or_else(|| PackError::InvalidIndex("index bounds overflow".to_owned()))?;
        if index_offset < data_offset
            || index_end > footer_start as u64
            || index_end != footer_start as u64
        {
            return Err(PackError::InvalidIndex(
                "index is outside the pack".to_owned(),
            ));
        }
        let index_start = usize::try_from(index_offset)
            .map_err(|_| PackError::InvalidIndex("index offset is too large".to_owned()))?;
        let index_end_usize = usize::try_from(index_end)
            .map_err(|_| PackError::InvalidIndex("index end is too large".to_owned()))?;
        let expected_index_hash = &bytes[footer_start + 40..footer_start + 72];
        let actual_index_hash = blake3::hash(&bytes[index_start..index_end_usize]);
        if expected_index_hash != actual_index_hash.as_bytes() {
            return Err(PackError::InvalidIndex("index digest mismatch".to_owned()));
        }

        let mut entries = Vec::with_capacity(
            usize::try_from(entry_count)
                .map_err(|_| PackError::InvalidIndex("too many entries".to_owned()))?,
        );
        let mut previous_hash = None::<String>;
        for position in 0..entry_count {
            let entry_offset =
                index_offset
                    .checked_add(position.checked_mul(ENTRY_SIZE as u64).ok_or_else(|| {
                        PackError::InvalidIndex("entry offset overflows".to_owned())
                    })?)
                    .ok_or_else(|| PackError::InvalidIndex("entry offset overflows".to_owned()))?;
            let start = usize::try_from(entry_offset)
                .map_err(|_| PackError::InvalidIndex("entry offset is too large".to_owned()))?;
            let end = start
                .checked_add(ENTRY_SIZE)
                .ok_or_else(|| PackError::InvalidIndex("entry end overflows".to_owned()))?;
            let encoded_hash = read_hash(&bytes[start..end], 0)?;
            let raw_hash = read_hash(&bytes[start..end], 32)?;
            let entry = PackEntry {
                encoded_hash,
                raw_hash,
                offset: read_u64(&bytes[start..end], 64).ok_or(PackError::Truncated)?,
                encoded_length: read_u64(&bytes[start..end], 72).ok_or(PackError::Truncated)?,
                raw_length: read_u64(&bytes[start..end], 80).ok_or(PackError::Truncated)?,
                compression: read_u32(&bytes[start..end], 88).ok_or(PackError::Truncated)?,
                flags: read_u32(&bytes[start..end], 92).ok_or(PackError::Truncated)?,
            };
            validate_hash("encoded", &entry.encoded_hash)?;
            validate_hash("raw", &entry.raw_hash)?;
            if entry.raw_length > MAX_DECLARED_CHUNK_BYTES
                || entry.encoded_length > MAX_DECLARED_CHUNK_BYTES
            {
                return Err(PackError::InvalidIndex(
                    "declared chunk length is too large".to_owned(),
                ));
            }
            if entry.compression != COMPRESSION_ZSTD {
                return Err(PackError::UnsupportedCompression(entry.compression));
            }
            if previous_hash
                .as_ref()
                .is_some_and(|previous| previous >= &entry.encoded_hash)
            {
                return Err(PackError::InvalidIndex(
                    "entries are not strictly sorted or contain a duplicate".to_owned(),
                ));
            }
            previous_hash = Some(entry.encoded_hash.clone());
            let end_offset = entry.encoded_end()?;
            if entry.offset < data_offset || end_offset > index_offset {
                return Err(PackError::InvalidIndex(
                    "chunk bytes are outside the data region".to_owned(),
                ));
            }
            entries.push(entry);
        }
        let mut by_offset = entries.clone();
        by_offset.sort_by_key(|entry| entry.offset);
        for pair in by_offset.windows(2) {
            if pair[0].encoded_end()? > pair[1].offset {
                return Err(PackError::InvalidIndex(
                    "chunk data ranges overlap".to_owned(),
                ));
            }
        }
        Ok(Self {
            bytes,
            entries,
            index_offset,
        })
    }

    pub fn verify_pack_hash(&self, expected: &str) -> Result<(), PackError> {
        validate_hash("pack", expected)?;
        let actual = blake3::hash(self.bytes).to_hex().to_string();
        if actual != expected {
            return Err(PackError::PackHashMismatch {
                expected: expected.to_owned(),
                actual,
            });
        }
        Ok(())
    }

    pub fn entries(&self) -> &[PackEntry] {
        &self.entries
    }

    pub fn entry(&self, encoded_hash: &str) -> Result<&PackEntry, PackError> {
        validate_hash("encoded", encoded_hash)?;
        self.entries
            .binary_search_by(|entry| entry.encoded_hash.as_str().cmp(encoded_hash))
            .map(|index| &self.entries[index])
            .map_err(|_| PackError::ChunkNotFound(encoded_hash.to_owned()))
    }

    pub fn read_encoded(&self, encoded_hash: &str) -> Result<Vec<u8>, PackError> {
        let entry = self.entry(encoded_hash)?;
        let start = usize::try_from(entry.offset)
            .map_err(|_| PackError::InvalidIndex("chunk offset is too large".to_owned()))?;
        let length = usize::try_from(entry.encoded_length)
            .map_err(|_| PackError::InvalidIndex("chunk length is too large".to_owned()))?;
        let end = start
            .checked_add(length)
            .ok_or_else(|| PackError::InvalidIndex("chunk end overflows".to_owned()))?;
        let bytes = self
            .bytes
            .get(start..end)
            .ok_or(PackError::Truncated)?
            .to_vec();
        let actual = blake3::hash(&bytes).to_hex().to_string();
        if actual != entry.encoded_hash {
            return Err(PackError::ChunkHashMismatch {
                expected: entry.encoded_hash.clone(),
                actual,
            });
        }
        Ok(bytes)
    }

    pub fn read_raw(&self, encoded_hash: &str) -> Result<Vec<u8>, PackError> {
        let entry = self.entry(encoded_hash)?.clone();
        let encoded = self.read_encoded(encoded_hash)?;
        let raw_length = usize::try_from(entry.raw_length)
            .map_err(|_| PackError::InvalidIndex("raw length is too large".to_owned()))?;
        let raw = match entry.compression {
            COMPRESSION_ZSTD => zstd::bulk::decompress(&encoded, raw_length)
                .map_err(|error| PackError::Decompression(error.to_string()))?,
            compression => return Err(PackError::UnsupportedCompression(compression)),
        };
        if raw.len() != raw_length {
            return Err(PackError::ChunkSizeMismatch);
        }
        let actual = blake3::hash(&raw).to_hex().to_string();
        if actual != entry.raw_hash {
            return Err(PackError::RawHashMismatch {
                expected: entry.raw_hash,
                actual,
            });
        }
        Ok(raw)
    }

    pub fn index_offset(&self) -> u64 {
        self.index_offset
    }
}

fn write_hash(bytes: &mut Vec<u8>, value: &str) -> Result<(), PackError> {
    let decoded = decode_hash(value)?;
    bytes.extend_from_slice(&decoded);
    Ok(())
}

fn read_hash(bytes: &[u8], offset: usize) -> Result<String, PackError> {
    let end = offset.checked_add(32).ok_or(PackError::Truncated)?;
    let value = bytes.get(offset..end).ok_or(PackError::Truncated)?;
    Ok(value.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn decode_hash(value: &str) -> Result<[u8; 32], PackError> {
    validate_hash("hash", value)?;
    let mut output = [0u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        output[index] = (hex_value(pair[0])? << 4) | hex_value(pair[1])?;
    }
    Ok(output)
}

fn hex_value(value: u8) -> Result<u8, PackError> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        _ => Err(PackError::Configuration(
            "hash must be lowercase hexadecimal".to_owned(),
        )),
    }
}

fn validate_hash(field: &str, value: &str) -> Result<(), PackError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(PackError::Configuration(format!(
            "{field} hash must be 64 lowercase hexadecimal characters"
        )));
    }
    Ok(())
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        bytes.get(offset..offset.checked_add(2)?)?.try_into().ok()?,
    ))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn read_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        bytes.get(offset..offset.checked_add(8)?)?.try_into().ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn input(raw: &[u8], _salt: u8) -> PackInput {
        let encoded = zstd::bulk::compress(raw, 3).unwrap();
        PackInput::new(
            blake3::hash(&encoded).to_hex().to_string(),
            blake3::hash(raw).to_hex().to_string(),
            raw.len() as u64,
            encoded,
        )
    }

    #[test]
    fn round_trips_sorted_entries_and_raw_bytes() {
        let mut builder = PackBuilder::new(PackConfig {
            target_bytes: 1024,
            min_bytes: 1,
            max_bytes: 1024 * 1024,
        })
        .unwrap();
        let first = input(b"hello", 1);
        let second = input(b"world", 2);
        builder.push(second.clone()).unwrap();
        builder.push(first.clone()).unwrap();
        let artifacts = builder.build().unwrap();
        assert_eq!(artifacts.len(), 1);
        let artifact = &artifacts[0];
        let reader = PackReader::parse(&artifact.bytes).unwrap();
        reader.verify_pack_hash(&artifact.pack_hash).unwrap();
        assert_eq!(reader.entries().len(), 2);
        assert_eq!(reader.read_raw(&first.encoded_hash).unwrap(), b"hello");
        assert_eq!(reader.read_raw(&second.encoded_hash).unwrap(), b"world");
    }

    #[test]
    fn streams_file_backed_packs_and_verifies_the_written_bytes() {
        let root = std::env::temp_dir().join(format!("launcher-packs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let mut inputs = Vec::new();
        for raw in [b"hello".as_slice(), b"world".as_slice()] {
            let encoded = zstd::bulk::compress(raw, 3).unwrap();
            let encoded_hash = blake3::hash(&encoded).to_hex().to_string();
            let encoded_path = root.join(format!("{encoded_hash}.bin"));
            fs::write(&encoded_path, &encoded).unwrap();
            inputs.push(PackFileInput::new(
                encoded_hash,
                blake3::hash(raw).to_hex().to_string(),
                raw.len() as u64,
                encoded.len() as u64,
                encoded_path,
            ));
        }

        let artifacts = write_packs_from_files(
            inputs,
            PackConfig {
                target_bytes: 1024,
                min_bytes: 1,
                max_bytes: 1024 * 1024,
            },
            root.join("packs"),
        )
        .unwrap();
        assert_eq!(artifacts.len(), 1);
        let artifact = &artifacts[0];
        let bytes = fs::read(
            root.join("packs")
                .join(format!("{}.pack", artifact.pack_hash)),
        )
        .unwrap();
        let reader = PackReader::parse(&bytes).unwrap();
        reader.verify_pack_hash(&artifact.pack_hash).unwrap();
        assert_eq!(reader.entries().len(), 2);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_header_index_and_footer_corruption() {
        let artifact = build_packs(
            [input(b"payload", 1)],
            PackConfig {
                target_bytes: 1024,
                min_bytes: 1,
                max_bytes: 1024 * 1024,
            },
        )
        .unwrap()
        .remove(0);
        let mut header = artifact.bytes.clone();
        header[12] = 0;
        assert!(PackReader::parse(&header).is_err());
        let mut index = artifact.bytes.clone();
        index[artifact.bytes.len() - FOOTER_SIZE - 1] ^= 1;
        assert!(PackReader::parse(&index).is_err());
        let mut footer = artifact.bytes;
        let footer_last = footer.len() - 1;
        footer[footer_last] ^= 1;
        assert!(PackReader::parse(&footer).is_err());
    }

    #[test]
    fn rejects_invalid_offsets_overlap_duplicate_and_truncation() {
        let a = input(b"a", 1);
        let b = input(b"b", 2);
        let artifact = build_packs(
            [a, b],
            PackConfig {
                target_bytes: 1024,
                min_bytes: 1,
                max_bytes: 1024 * 1024,
            },
        )
        .unwrap()
        .remove(0);
        let reader = PackReader::parse(&artifact.bytes).unwrap();
        let index = reader.index_offset() as usize;
        let mut offset = artifact.bytes.clone();
        offset[index + 64..index + 72].copy_from_slice(&0u64.to_le_bytes());
        rewrite_index_digest(&mut offset);
        assert!(PackReader::parse(&offset).is_err());

        let mut overlap = artifact.bytes.clone();
        let first_offset = u64::from_le_bytes(overlap[index + 64..index + 72].try_into().unwrap());
        overlap[index + ENTRY_SIZE + 64..index + ENTRY_SIZE + 72]
            .copy_from_slice(&first_offset.to_le_bytes());
        rewrite_index_digest(&mut overlap);
        assert!(PackReader::parse(&overlap).is_err());

        let mut duplicate = artifact.bytes.clone();
        let first_hash = duplicate[index..index + 32].to_vec();
        duplicate[index + ENTRY_SIZE..index + ENTRY_SIZE + 32].copy_from_slice(&first_hash);
        rewrite_index_digest(&mut duplicate);
        assert!(PackReader::parse(&duplicate).is_err());
        assert!(PackReader::parse(&artifact.bytes[..artifact.bytes.len() - 1]).is_err());
    }

    #[test]
    fn rejects_wrong_pack_hash_chunk_hash_and_oversized_declared_lengths() {
        let artifact = build_packs(
            [input(b"payload", 1)],
            PackConfig {
                target_bytes: 1024,
                min_bytes: 1,
                max_bytes: 1024 * 1024,
            },
        )
        .unwrap()
        .remove(0);
        let reader = PackReader::parse(&artifact.bytes).unwrap();
        assert!(reader.verify_pack_hash(&"0".repeat(64)).is_err());
        let index = reader.index_offset() as usize;
        let mut chunk = artifact.bytes.clone();
        chunk[reader.entries()[0].offset as usize] ^= 1;
        assert!(PackReader::parse(&chunk).is_ok());
        assert!(
            PackReader::parse(&chunk)
                .unwrap()
                .read_encoded(&reader.entries()[0].encoded_hash)
                .is_err()
        );

        let mut oversized = artifact.bytes;
        oversized[index + 80..index + 88]
            .copy_from_slice(&(MAX_DECLARED_CHUNK_BYTES + 1).to_le_bytes());
        rewrite_index_digest(&mut oversized);
        assert!(PackReader::parse(&oversized).is_err());
    }

    fn rewrite_index_digest(bytes: &mut [u8]) {
        let footer_start = bytes.len() - FOOTER_SIZE;
        let index_offset = u64::from_le_bytes(
            bytes[footer_start + 12..footer_start + 20]
                .try_into()
                .unwrap(),
        ) as usize;
        let index_length = u64::from_le_bytes(
            bytes[footer_start + 20..footer_start + 28]
                .try_into()
                .unwrap(),
        ) as usize;
        let digest = blake3::hash(&bytes[index_offset..index_offset + index_length]);
        bytes[footer_start + 40..footer_start + 72].copy_from_slice(digest.as_bytes());
    }
}
