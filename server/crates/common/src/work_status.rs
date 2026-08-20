use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, ErrorKind},
    path::{Path, PathBuf},
    time::Duration,
};
use uuid::Uuid;

pub const WORK_STATUS_SCHEMA_VERSION: u32 = 1;
const MAX_WORK_STATUS_FILE_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkStatus {
    pub schema_version: u32,
    pub id: String,
    pub kind: String,
    pub state: String,
    pub game: Option<String>,
    pub version: Option<String>,
    pub provider: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    pub detail: String,
    pub progress_percent: Option<f32>,
    #[serde(default)]
    pub bytes_completed: Option<u64>,
    #[serde(default)]
    pub bytes_total: Option<u64>,
    #[serde(default)]
    pub rate_bytes_per_second: Option<u64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl WorkStatus {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: impl Into<String>,
        kind: impl Into<String>,
        state: impl Into<String>,
        game: Option<String>,
        version: Option<String>,
        provider: Option<String>,
        detail: impl Into<String>,
        progress_percent: Option<f32>,
    ) -> Self {
        let now = Utc::now();
        Self {
            schema_version: WORK_STATUS_SCHEMA_VERSION,
            id: id.into(),
            kind: kind.into(),
            state: state.into(),
            game,
            version,
            provider,
            source: None,
            detail: detail.into(),
            progress_percent,
            bytes_completed: None,
            bytes_total: None,
            rate_bytes_per_second: None,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn touch(&mut self) {
        self.updated_at = Utc::now();
    }

    pub fn is_active(&self) -> bool {
        !matches!(self.state.as_str(), "DONE" | "FAILED" | "CANCELLED")
    }
}

#[derive(Debug, Clone)]
pub struct WorkStatusStore {
    directory: PathBuf,
}

impl WorkStatusStore {
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }

    pub fn write(&self, status: &WorkStatus) -> io::Result<()> {
        let file_name = status_file_name(&status.id)?;
        let bytes = serde_json::to_vec_pretty(status)
            .map_err(|error| io::Error::new(ErrorKind::InvalidData, error))?;
        if bytes.len() as u64 > MAX_WORK_STATUS_FILE_BYTES {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                "work status record exceeds the configured size limit",
            ));
        }
        fs::create_dir_all(&self.directory)?;
        let target = self.directory.join(file_name);
        let temporary = self.directory.join(format!(
            ".{}.{}.{}.tmp",
            status.id,
            std::process::id(),
            Uuid::new_v4()
        ));
        fs::write(&temporary, bytes)?;
        // The status is advisory. Replacing it with a tiny gap is preferable
        // to leaving a half-written JSON document for the public API to read.
        match fs::remove_file(&target) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        match fs::rename(&temporary, target) {
            Ok(()) => Ok(()),
            Err(error) => {
                let _ = fs::remove_file(temporary);
                Err(error)
            }
        }
    }

    pub fn remove(&self, id: &str) -> io::Result<()> {
        let target = self.directory.join(status_file_name(id)?);
        match fs::remove_file(&target) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    pub fn read_active(&self, max_age: Duration) -> io::Result<Vec<WorkStatus>> {
        let entries = match fs::read_dir(&self.directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let now = Utc::now();
        let max_age = chrono::Duration::from_std(max_age).unwrap_or(chrono::Duration::MAX);
        let mut statuses = Vec::new();
        for entry in entries {
            let entry = entry?;
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => continue,
            };
            if !file_type.is_file()
                || file_type.is_symlink()
                || entry.path().extension().and_then(|value| value.to_str()) != Some("json")
            {
                continue;
            }
            if entry
                .metadata()
                .map(|metadata| metadata.len() > MAX_WORK_STATUS_FILE_BYTES)
                .unwrap_or(true)
            {
                continue;
            }
            let bytes = match fs::read(entry.path()) {
                Ok(bytes) => bytes,
                Err(_) => continue,
            };
            let status: WorkStatus = match serde_json::from_slice(&bytes) {
                Ok(status) => status,
                Err(_) => continue,
            };
            if status.schema_version != WORK_STATUS_SCHEMA_VERSION || !status.is_active() {
                continue;
            }
            let age = now.signed_duration_since(status.updated_at);
            if age >= chrono::Duration::zero() && age <= max_age {
                statuses.push(status);
            }
        }
        statuses.sort_by_key(|status| status.updated_at);
        Ok(statuses)
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }
}

fn status_file_name(id: &str) -> io::Result<String> {
    if id.is_empty()
        || id.len() > 128
        || !id.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
    {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            "work status id contains unsupported characters",
        ));
    }
    Ok(format!("{id}.json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn test_directory() -> PathBuf {
        std::env::temp_dir().join(format!("launcher-work-status-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn writes_reads_and_removes_active_status() {
        let directory = test_directory();
        let store = WorkStatusStore::new(&directory);
        let status = WorkStatus::new(
            "scrape-1",
            "SCRAPER",
            "DOWNLOADING",
            Some("OpenTTD".to_owned()),
            Some("15.3".to_owned()),
            None,
            "Downloading release",
            Some(42.0),
        );

        store.write(&status).unwrap();
        let active = store.read_active(Duration::from_secs(60)).unwrap();
        assert_eq!(active, vec![status]);

        store.remove("scrape-1").unwrap();
        assert!(
            store
                .read_active(Duration::from_secs(60))
                .unwrap()
                .is_empty()
        );
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn ignores_terminal_and_stale_records() {
        let directory = test_directory();
        let store = WorkStatusStore::new(&directory);
        let mut terminal = WorkStatus::new(
            "terminal",
            "SCRAPER",
            "DONE",
            Some("Luanti".to_owned()),
            None,
            None,
            "Done",
            None,
        );
        terminal.updated_at = Utc::now();
        store.write(&terminal).unwrap();
        let mut stale = WorkStatus::new(
            "stale",
            "REUPLOAD",
            "REUPLOADING",
            None,
            None,
            Some("filemirage".to_owned()),
            "Reuploading",
            None,
        );
        stale.updated_at = Utc::now() - chrono::Duration::hours(2);
        store.write(&stale).unwrap();

        assert!(
            store
                .read_active(Duration::from_secs(60))
                .unwrap()
                .is_empty()
        );
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn rejects_oversized_records_before_writing_them() {
        let directory = test_directory();
        let store = WorkStatusStore::new(&directory);
        let status = WorkStatus::new(
            "oversized",
            "SCRAPER",
            "DOWNLOADING",
            None,
            None,
            None,
            "x".repeat((MAX_WORK_STATUS_FILE_BYTES + 1) as usize),
            Some(1.0),
        );

        assert!(store.write(&status).is_err());
        assert!(!directory.exists());
    }
}
