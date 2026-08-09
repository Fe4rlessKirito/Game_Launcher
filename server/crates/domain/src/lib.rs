use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BuildState {
    Discovered,
    Analyzed,
    Packaged,
    Uploaded,
    Verified,
    Ready,
    Published,
    Failed,
}

#[derive(Debug, Error, PartialEq, Eq)]
#[error("invalid build transition from {from:?} to {to:?}")]
pub struct InvalidTransition {
    pub from: BuildState,
    pub to: BuildState,
}

impl BuildState {
    pub fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Discovered, Self::Analyzed)
                | (Self::Analyzed, Self::Packaged)
                | (Self::Packaged, Self::Uploaded)
                | (Self::Uploaded, Self::Verified)
                | (Self::Verified, Self::Ready)
                | (Self::Ready, Self::Published)
                | (_, Self::Failed)
        )
    }

    pub fn transition(self, next: Self) -> Result<Self, InvalidTransition> {
        if self.can_transition_to(next) {
            Ok(next)
        } else {
            Err(InvalidTransition {
                from: self,
                to: next,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publication_requires_all_stages() {
        assert!(!BuildState::Analyzed.can_transition_to(BuildState::Published));
        assert_eq!(
            BuildState::Ready.transition(BuildState::Published).unwrap(),
            BuildState::Published
        );
    }
}
