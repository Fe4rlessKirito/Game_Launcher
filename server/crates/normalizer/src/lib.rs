//! Safe, bounded normalization of operator-supplied game archives.
//!
//! The packager intentionally consumes directories only. This crate is the
//! boundary that turns ZIP/RAR/7z/TAR inputs into a temporary canonical
//! directory while rejecting path traversal, links, duplicate entries, and
//! decompression bombs before analysis or chunking starts.

use anyhow::{Context, Result, bail};
use bzip2::read::BzDecoder;
use flate2::read::GzDecoder;
use sevenz_rust::SevenZArchiveEntry;
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use tar::EntryType;
use unrar::Archive;
use uuid::Uuid;
use zip::ZipArchive;

const DEFAULT_MAX_ARCHIVE_BYTES: u64 = 2 * 1024 * 1024 * 1024 * 1024;
const DEFAULT_MAX_OUTPUT_BYTES: u64 = 4 * 1024 * 1024 * 1024 * 1024;
const DEFAULT_MAX_FILE_BYTES: u64 = 512 * 1024 * 1024 * 1024;
const DEFAULT_MAX_ENTRIES: u64 = 2_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputFormat {
    Directory,
    Zip,
    Rar,
    SevenZip,
    Tar,
    TarGzip,
    TarBzip2,
}

impl InputFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Directory => "directory",
            Self::Zip => "zip",
            Self::Rar => "rar",
            Self::SevenZip => "7z",
            Self::Tar => "tar",
            Self::TarGzip => "tar.gz",
            Self::TarBzip2 => "tar.bz2",
        }
    }
}

#[derive(Debug, Clone)]
pub struct NormalizationLimits {
    pub max_archive_bytes: u64,
    pub max_output_bytes: u64,
    pub max_file_bytes: u64,
    pub max_entries: u64,
}

impl Default for NormalizationLimits {
    fn default() -> Self {
        Self {
            max_archive_bytes: DEFAULT_MAX_ARCHIVE_BYTES,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
            max_entries: DEFAULT_MAX_ENTRIES,
        }
    }
}

impl NormalizationLimits {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            max_archive_bytes: env_u64(
                "LAUNCHER_NORMALIZER_MAX_ARCHIVE_BYTES",
                DEFAULT_MAX_ARCHIVE_BYTES,
            )?,
            max_output_bytes: env_u64(
                "LAUNCHER_NORMALIZER_MAX_OUTPUT_BYTES",
                DEFAULT_MAX_OUTPUT_BYTES,
            )?,
            max_file_bytes: env_u64("LAUNCHER_NORMALIZER_MAX_FILE_BYTES", DEFAULT_MAX_FILE_BYTES)?,
            max_entries: env_u64("LAUNCHER_NORMALIZER_MAX_ENTRIES", DEFAULT_MAX_ENTRIES)?,
        })
    }
}

#[derive(Debug)]
pub struct NormalizedInput {
    pub root: PathBuf,
    pub format: InputFormat,
    cleanup_root: Option<PathBuf>,
}

impl NormalizedInput {
    pub fn cleanup(self) -> Result<()> {
        if let Some(root) = self.cleanup_root {
            fs::remove_dir_all(&root)
                .with_context(|| format!("could not remove normalized input {}", root.display()))?;
        }
        Ok(())
    }
}

#[derive(Debug, Default)]
struct ExtractionState {
    entries: u64,
    output_bytes: u64,
    paths: HashSet<PathBuf>,
}

impl ExtractionState {
    fn entry(&mut self, limits: &NormalizationLimits, path: &Path) -> Result<()> {
        self.entries = self.entries.saturating_add(1);
        if self.entries > limits.max_entries {
            bail!("archive contains more than {} entries", limits.max_entries);
        }
        if !self.paths.insert(path.to_path_buf()) {
            bail!("archive contains duplicate entry {}", path.display());
        }
        Ok(())
    }

    fn file_size(&mut self, limits: &NormalizationLimits, size: u64) -> Result<()> {
        if size > limits.max_file_bytes {
            bail!(
                "archive entry is {} bytes, above the per-file limit of {} bytes",
                size,
                limits.max_file_bytes
            );
        }
        self.output_bytes = self
            .output_bytes
            .checked_add(size)
            .context("archive output size overflow")?;
        if self.output_bytes > limits.max_output_bytes {
            bail!(
                "archive expands beyond the output limit of {} bytes",
                limits.max_output_bytes
            );
        }
        Ok(())
    }
}

pub fn normalize_input(input: &Path, limits: &NormalizationLimits) -> Result<NormalizedInput> {
    if input.is_dir() {
        return Ok(NormalizedInput {
            root: input.to_path_buf(),
            format: InputFormat::Directory,
            cleanup_root: None,
        });
    }
    if !input.is_file() {
        bail!(
            "ingest input {} is not a directory or regular file",
            input.display()
        );
    }
    let metadata = fs::metadata(input)?;
    if metadata.len() > limits.max_archive_bytes {
        bail!(
            "archive is {} bytes, above the input limit of {} bytes",
            metadata.len(),
            limits.max_archive_bytes
        );
    }

    let format = detect_format(input)?;
    if format == InputFormat::Directory {
        bail!(
            "{} is not a supported ZIP, RAR, 7z, or TAR archive",
            input.display()
        );
    }

    let temp_parent = std::env::var_os("LAUNCHER_NORMALIZER_TEMP_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("launcher-normalized");
    fs::create_dir_all(&temp_parent)?;
    let extraction_root = temp_parent.join(Uuid::new_v4().to_string());
    fs::create_dir(&extraction_root)?;

    let result = extract_archive(input, format, &extraction_root, limits);
    if let Err(error) = result {
        let _ = fs::remove_dir_all(&extraction_root);
        return Err(error);
    }

    let root = collapse_single_root(&extraction_root)?;
    Ok(NormalizedInput {
        root,
        format,
        cleanup_root: Some(extraction_root),
    })
}

fn detect_format(input: &Path) -> Result<InputFormat> {
    let mut file = File::open(input)?;
    let mut header = [0_u8; 512];
    let count = file.read(&mut header)?;
    let header = &header[..count];
    if header.starts_with(b"PK\x03\x04")
        || header.starts_with(b"PK\x05\x06")
        || header.starts_with(b"PK\x07\x08")
    {
        return Ok(InputFormat::Zip);
    }
    if header.starts_with(b"7z\xBC\xAF\x27\x1C") {
        return Ok(InputFormat::SevenZip);
    }
    if header.starts_with(b"Rar!\x1A\x07\x00") || header.starts_with(b"Rar!\x1A\x07\x01\x00") {
        return Ok(InputFormat::Rar);
    }
    if header.starts_with(b"\x1F\x8B") {
        return Ok(InputFormat::TarGzip);
    }
    if header.starts_with(b"BZh") {
        return Ok(InputFormat::TarBzip2);
    }
    if header.len() >= 265 && &header[257..262] == b"ustar" {
        return Ok(InputFormat::Tar);
    }

    let tar_stem = input
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(|stem| stem.to_ascii_lowercase())
        .is_some_and(|stem| stem.ends_with(".tar"));
    match input
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
    {
        Some(extension) if extension == "zip" => Ok(InputFormat::Zip),
        Some(extension) if extension == "rar" => Ok(InputFormat::Rar),
        Some(extension) if extension == "7z" => Ok(InputFormat::SevenZip),
        Some(extension) if extension == "tar" => Ok(InputFormat::Tar),
        Some(extension) if extension == "gz" && tar_stem => Ok(InputFormat::TarGzip),
        Some(extension) if extension == "bz2" && tar_stem => Ok(InputFormat::TarBzip2),
        _ => Ok(InputFormat::Directory),
    }
}

fn extract_archive(
    input: &Path,
    format: InputFormat,
    destination: &Path,
    limits: &NormalizationLimits,
) -> Result<()> {
    match format {
        InputFormat::Zip => extract_zip(input, destination, limits),
        InputFormat::Rar => extract_rar(input, destination, limits),
        InputFormat::SevenZip => extract_seven_zip(input, destination, limits),
        InputFormat::Tar => extract_tar(File::open(input)?, destination, limits),
        InputFormat::TarGzip => {
            extract_tar(GzDecoder::new(File::open(input)?), destination, limits)
        }
        InputFormat::TarBzip2 => {
            extract_tar(BzDecoder::new(File::open(input)?), destination, limits)
        }
        InputFormat::Directory => bail!("directories do not need normalization"),
    }
}

fn extract_zip(input: &Path, destination: &Path, limits: &NormalizationLimits) -> Result<()> {
    let file = File::open(input)?;
    let mut archive = ZipArchive::new(file).context("could not read ZIP archive")?;
    let mut state = ExtractionState::default();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let relative = safe_relative_path(entry.name())?;
        state.entry(limits, &relative)?;
        if entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            bail!("ZIP symlink entry {} is not allowed", entry.name());
        }
        let target = destination.join(&relative);
        if entry.is_dir() {
            fs::create_dir_all(&target)?;
            continue;
        }
        let declared_size = entry.size();
        state.file_size(limits, declared_size)?;
        extract_reader(&mut entry, &target, declared_size)?;
    }
    Ok(())
}

fn extract_tar<R: Read>(reader: R, destination: &Path, limits: &NormalizationLimits) -> Result<()> {
    let mut archive = tar::Archive::new(reader);
    let mut state = ExtractionState::default();
    for item in archive.entries()? {
        let mut entry = item?;
        let raw_path = entry.path()?.to_path_buf();
        let raw_text = raw_path.to_string_lossy().replace('\\', "/");
        if matches!(raw_text.as_str(), "." | "./" | "") {
            if entry.header().entry_type() == EntryType::dir() {
                continue;
            }
            bail!("TAR file entry has no usable path");
        }
        let relative = safe_relative_path(raw_path)?;
        state.entry(limits, &relative)?;
        let entry_type = entry.header().entry_type();
        if entry_type == EntryType::dir() {
            fs::create_dir_all(destination.join(relative))?;
            continue;
        }
        if entry_type.is_symlink() || entry_type.is_hard_link() {
            bail!("TAR link entry {} is not allowed", relative.display());
        }
        if !entry_type.is_file() {
            bail!("TAR entry {} has unsupported type", relative.display());
        }
        let size = entry.header().size()?;
        state.file_size(limits, size)?;
        extract_reader(&mut entry, &destination.join(relative), size)?;
    }
    Ok(())
}

fn extract_seven_zip(input: &Path, destination: &Path, limits: &NormalizationLimits) -> Result<()> {
    let mut state = ExtractionState::default();
    sevenz_rust::decompress_with_extract_fn(
        File::open(input)?,
        destination,
        |entry: &SevenZArchiveEntry, reader: &mut dyn Read, _default_path: &PathBuf| {
            let relative = safe_relative_path(entry.name()).map_err(to_sevenz_error)?;
            state.entry(limits, &relative).map_err(to_sevenz_error)?;
            if entry.is_anti_item() {
                return Err(sevenz_rust::Error::other(
                    "7z anti-item entries are not allowed",
                ));
            }
            let target = destination.join(relative);
            if entry.is_directory() {
                fs::create_dir_all(target).map_err(sevenz_rust::Error::io)?;
            } else {
                state
                    .file_size(limits, entry.size())
                    .map_err(to_sevenz_error)?;
                extract_reader(reader, &target, entry.size()).map_err(to_sevenz_error)?;
            }
            Ok(true)
        },
    )
    .context("could not extract 7z archive")
}

fn extract_rar(input: &Path, destination: &Path, limits: &NormalizationLimits) -> Result<()> {
    let mut archive = Archive::new(input)
        .open_for_processing()
        .context("could not open RAR archive")?;
    let mut state = ExtractionState::default();
    loop {
        let Some(cursor) = archive.read_header()? else {
            break;
        };
        let header = cursor.entry();
        let relative = safe_relative_path(&header.filename)?;
        state.entry(limits, &relative)?;
        let target = destination.join(&relative);
        if header.is_directory() {
            fs::create_dir_all(&target)?;
            archive = cursor.skip()?;
            continue;
        }
        if header.is_split() {
            bail!(
                "RAR split entry {} is not supported in one-file normalization",
                relative.display()
            );
        }
        state.file_size(limits, header.unpacked_size)?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        archive = cursor.extract_to(&target)?;
    }
    Ok(())
}

fn extract_reader(reader: &mut dyn Read, target: &Path, declared_size: u64) -> Result<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)
        .with_context(|| format!("could not create extracted file {}", target.display()))?;
    let copied = io::copy(reader, &mut output)?;
    if copied != declared_size {
        bail!(
            "extracted {} bytes for {}, expected {}",
            copied,
            target.display(),
            declared_size
        );
    }
    output.flush()?;
    Ok(())
}

fn safe_relative_path(raw: impl AsRef<Path>) -> Result<PathBuf> {
    let raw = raw.as_ref();
    let normalized = raw.to_string_lossy().replace('\\', "/");
    if normalized.is_empty() || normalized.contains('\0') {
        bail!("archive entry has an empty or invalid name");
    }
    let bytes = normalized.as_bytes();
    if normalized.starts_with('/')
        || normalized.starts_with("//")
        || (bytes.len() >= 2 && bytes[1] == b':')
    {
        bail!("archive entry {normalized:?} is an absolute or drive-qualified path");
    }
    let path = Path::new(&normalized);
    let mut safe = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => safe.push(value),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                bail!("archive entry {normalized:?} escapes the extraction root")
            }
        }
    }
    if safe.as_os_str().is_empty() {
        bail!("archive entry has no usable path");
    }
    Ok(safe)
}

fn collapse_single_root(extraction_root: &Path) -> Result<PathBuf> {
    let mut entries = fs::read_dir(extraction_root)?.collect::<Result<Vec<_>, _>>()?;
    if entries.len() == 1 && entries[0].path().is_dir() {
        return Ok(entries.remove(0).path());
    }
    Ok(extraction_root.to_path_buf())
}

fn to_sevenz_error(error: anyhow::Error) -> sevenz_rust::Error {
    sevenz_rust::Error::io(io::Error::new(
        io::ErrorKind::InvalidData,
        error.to_string(),
    ))
}

fn env_u64(name: &str, default: u64) -> Result<u64> {
    std::env::var(name)
        .unwrap_or_else(|_| default.to_string())
        .parse::<u64>()
        .with_context(|| format!("{name} must be an unsigned integer"))
}

#[cfg(test)]
mod tests {
    use super::safe_relative_path;
    use std::path::Path;

    #[test]
    fn rejects_escape_paths() {
        assert!(safe_relative_path(Path::new("../outside.txt")).is_err());
        assert!(safe_relative_path(Path::new("C:\\outside.txt")).is_err());
        assert!(safe_relative_path(Path::new("/outside.txt")).is_err());
    }

    #[test]
    fn normalizes_windows_separators() {
        assert_eq!(
            safe_relative_path(Path::new("game\\bin\\game.exe")).unwrap(),
            Path::new("game/bin/game.exe")
        );
    }
}
