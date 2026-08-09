use chrono::{DateTime, Utc};
use launcher_domain::BuildState;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestionProgress {
    pub state: BuildState,
    pub files_processed: u64,
    pub bytes_processed: u64,
    pub chunks_created: u64,
    pub chunks_reused: u64,
    pub updated_at: DateTime<Utc>,
}

impl IngestionProgress {
    pub fn new() -> Self {
        Self {
            state: BuildState::Discovered,
            files_processed: 0,
            bytes_processed: 0,
            chunks_created: 0,
            chunks_reused: 0,
            updated_at: Utc::now(),
        }
    }
    pub fn advance(&mut self, next: BuildState) -> Result<(), launcher_domain::InvalidTransition> {
        self.state = self.state.transition(next)?;
        self.updated_at = Utc::now();
        Ok(())
    }
}

impl Default for IngestionProgress {
    fn default() -> Self {
        Self::new()
    }
}
