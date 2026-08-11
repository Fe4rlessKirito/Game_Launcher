use crate::email::ProvisioningEmailEvent;
use crate::secrets::{SecretMaterial, SecretStore};
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use rand::{RngCore, rngs::OsRng};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::Mutex;
use uuid::Uuid;

pub use crate::secrets::SecretRef;
pub use launcher_storage::ProvisioningMode;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ProvisioningError {
    #[error("provisioning configuration error: {0}")]
    Configuration(String),
    #[error("provisioning job was not found")]
    NotFound,
    #[error("invalid provisioning transition from {from} using {event}")]
    InvalidTransition { from: String, event: String },
    #[error("provisioning conflict: {0}")]
    Conflict(String),
    #[error("provisioning security error: {0}")]
    Security(String),
    #[error("provisioning mail error: {0}")]
    Mail(String),
    #[error("provisioning provider error: {0}")]
    Provider(String),
    #[error("provisioning secret error: {0}")]
    Secret(String),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ProvisioningStatus {
    #[serde(rename = "CREATED")]
    Created,
    #[serde(rename = "STARTING")]
    Starting,
    #[serde(rename = "REGISTRATION_STARTED")]
    RegistrationStarted,
    #[serde(rename = "WAITING_FOR_EMAIL")]
    WaitingForEmail,
    #[serde(rename = "EMAIL_RECEIVED")]
    EmailReceived,
    #[serde(rename = "WAITING_FOR_PROVIDER")]
    WaitingForProvider,
    #[serde(rename = "CANDIDATE_READY")]
    CandidateReady,
    #[serde(rename = "VALIDATING")]
    Validating,
    #[serde(rename = "ENROLLING")]
    Enrolling,
    #[serde(rename = "ENROLLED")]
    Enrolled,
    #[serde(rename = "FAILED_RETRYABLE")]
    FailedRetryable,
    #[serde(rename = "FAILED_PERMANENT")]
    FailedPermanent,
    #[serde(rename = "NEEDS_OPERATOR")]
    NeedsOperator,
    #[serde(rename = "CANCELLED")]
    Cancelled,
}

impl ProvisioningStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "CREATED",
            Self::Starting => "STARTING",
            Self::RegistrationStarted => "REGISTRATION_STARTED",
            Self::WaitingForEmail => "WAITING_FOR_EMAIL",
            Self::EmailReceived => "EMAIL_RECEIVED",
            Self::WaitingForProvider => "WAITING_FOR_PROVIDER",
            Self::CandidateReady => "CANDIDATE_READY",
            Self::Validating => "VALIDATING",
            Self::Enrolling => "ENROLLING",
            Self::Enrolled => "ENROLLED",
            Self::FailedRetryable => "FAILED_RETRYABLE",
            Self::FailedPermanent => "FAILED_PERMANENT",
            Self::NeedsOperator => "NEEDS_OPERATOR",
            Self::Cancelled => "CANCELLED",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Enrolled | Self::FailedPermanent | Self::Cancelled
        )
    }

    pub fn is_active(self) -> bool {
        !self.is_terminal()
    }
}

impl fmt::Display for ProvisioningStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl std::str::FromStr for ProvisioningStatus {
    type Err = ProvisioningError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_uppercase().as_str() {
            "CREATED" => Ok(Self::Created),
            "STARTING" => Ok(Self::Starting),
            "REGISTRATION_STARTED" => Ok(Self::RegistrationStarted),
            "WAITING_FOR_EMAIL" => Ok(Self::WaitingForEmail),
            "EMAIL_RECEIVED" => Ok(Self::EmailReceived),
            "WAITING_FOR_PROVIDER" => Ok(Self::WaitingForProvider),
            "CANDIDATE_READY" => Ok(Self::CandidateReady),
            "VALIDATING" => Ok(Self::Validating),
            "ENROLLING" => Ok(Self::Enrolling),
            "ENROLLED" => Ok(Self::Enrolled),
            "FAILED_RETRYABLE" => Ok(Self::FailedRetryable),
            "FAILED_PERMANENT" => Ok(Self::FailedPermanent),
            "NEEDS_OPERATOR" => Ok(Self::NeedsOperator),
            "CANCELLED" => Ok(Self::Cancelled),
            other => Err(ProvisioningError::Configuration(format!(
                "unknown provisioning status {other:?}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CapacityCandidate {
    pub provider_type: String,
    pub external_account_id: String,
    pub credential_reference: SecretRef,
    pub expected_capacity_bytes: u64,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProvisionRequest {
    pub provider_type: String,
    pub pool_id: String,
    pub requested_capacity_bytes: u64,
    pub expires_at: DateTime<Utc>,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProvisioningJob {
    pub id: Uuid,
    pub provider_type: String,
    pub pool_id: String,
    pub requested_capacity_bytes: u64,
    pub status: ProvisioningStatus,
    pub attempt_count: u32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub last_error_code: Option<String>,
    pub last_error_summary: Option<String>,
    pub inbound_email_token_hash: Option<String>,
    pub inbound_email_address: Option<String>,
    pub inbound_email_expires_at: Option<DateTime<Utc>>,
    pub candidate_reference: Option<String>,
    pub credential_reference: Option<SecretRef>,
    pub operator_action: Option<String>,
    pub retry_after: Option<DateTime<Utc>>,
    pub expires_at: DateTime<Utc>,
    pub idempotency_key: String,
}

impl ProvisioningJob {
    pub fn new(request: ProvisionRequest) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            provider_type: request.provider_type,
            pool_id: request.pool_id,
            requested_capacity_bytes: request.requested_capacity_bytes,
            status: ProvisioningStatus::Created,
            attempt_count: 0,
            created_at: now,
            updated_at: now,
            started_at: None,
            completed_at: None,
            last_error_code: None,
            last_error_summary: None,
            inbound_email_token_hash: None,
            inbound_email_address: None,
            inbound_email_expires_at: None,
            candidate_reference: None,
            credential_reference: None,
            operator_action: None,
            retry_after: None,
            expires_at: request.expires_at,
            idempotency_key: request.idempotency_key,
        }
    }

    pub fn request(&self) -> ProvisionRequest {
        ProvisionRequest {
            provider_type: self.provider_type.clone(),
            pool_id: self.pool_id.clone(),
            requested_capacity_bytes: self.requested_capacity_bytes,
            expires_at: self.expires_at,
            idempotency_key: self.idempotency_key.clone(),
        }
    }

    pub fn candidate(&self) -> Result<CapacityCandidate, ProvisioningError> {
        Ok(CapacityCandidate {
            provider_type: self.provider_type.clone(),
            external_account_id: self.candidate_reference.clone().ok_or_else(|| {
                ProvisioningError::Conflict("candidate reference is missing".to_owned())
            })?,
            credential_reference: self.credential_reference.clone().ok_or_else(|| {
                ProvisioningError::Conflict("credential reference is missing".to_owned())
            })?,
            expected_capacity_bytes: self.requested_capacity_bytes,
            metadata: BTreeMap::new(),
        })
    }

    pub fn apply_event(
        &mut self,
        event: ProvisioningEvent,
    ) -> Result<ProvisioningTransition, ProvisioningError> {
        self.apply_event_at(event, Utc::now())
    }

    pub fn apply_event_at(
        &mut self,
        event: ProvisioningEvent,
        now: DateTime<Utc>,
    ) -> Result<ProvisioningTransition, ProvisioningError> {
        let event_type = event.kind().to_owned();
        let from = self.status;
        let (to, safe_summary, candidate, retry_after) = match (&self.status, event) {
            (ProvisioningStatus::Created, ProvisioningEvent::Start) => {
                self.attempt_count = self.attempt_count.saturating_add(1);
                self.started_at.get_or_insert(now);
                (
                    ProvisioningStatus::Starting,
                    "job started".to_owned(),
                    None,
                    None,
                )
            }
            (
                ProvisioningStatus::Starting | ProvisioningStatus::RegistrationStarted,
                ProvisioningEvent::AwaitingEmail,
            ) => (
                ProvisioningStatus::WaitingForEmail,
                "waiting for provider email".to_owned(),
                None,
                None,
            ),
            (
                ProvisioningStatus::Starting,
                ProvisioningEvent::RegistrationStarted {
                    inbound_email_address,
                    inbound_email_expires_at,
                    inbound_email_token_hash,
                },
            ) => {
                self.inbound_email_address = Some(inbound_email_address);
                self.inbound_email_expires_at = Some(inbound_email_expires_at);
                self.inbound_email_token_hash = Some(inbound_email_token_hash);
                (
                    ProvisioningStatus::RegistrationStarted,
                    "provider registration started".to_owned(),
                    None,
                    None,
                )
            }
            (ProvisioningStatus::WaitingForEmail, ProvisioningEvent::EmailReceived { .. }) => (
                ProvisioningStatus::EmailReceived,
                "provisioning email accepted".to_owned(),
                None,
                None,
            ),
            (
                ProvisioningStatus::EmailReceived | ProvisioningStatus::WaitingForEmail,
                ProvisioningEvent::ProviderReady,
            ) => (
                ProvisioningStatus::WaitingForProvider,
                "provider is ready".to_owned(),
                None,
                None,
            ),
            (
                ProvisioningStatus::EmailReceived | ProvisioningStatus::WaitingForProvider,
                ProvisioningEvent::CandidateReady { candidate },
            ) => {
                self.candidate_reference = Some(candidate.external_account_id.clone());
                self.credential_reference = Some(candidate.credential_reference.clone());
                (
                    ProvisioningStatus::CandidateReady,
                    "candidate capacity returned".to_owned(),
                    Some(candidate),
                    None,
                )
            }
            (
                ProvisioningStatus::NeedsOperator,
                ProvisioningEvent::OperatorCompleted { candidate },
            ) => {
                self.candidate_reference = Some(candidate.external_account_id.clone());
                self.credential_reference = Some(candidate.credential_reference.clone());
                self.operator_action = None;
                (
                    ProvisioningStatus::CandidateReady,
                    "operator supplied a candidate reference".to_owned(),
                    Some(candidate),
                    None,
                )
            }
            (ProvisioningStatus::CandidateReady, ProvisioningEvent::ValidationStarted) => (
                ProvisioningStatus::Validating,
                "server validation started".to_owned(),
                None,
                None,
            ),
            (ProvisioningStatus::Validating, ProvisioningEvent::ValidationPassed) => (
                ProvisioningStatus::Enrolling,
                "server validation passed".to_owned(),
                None,
                None,
            ),
            (ProvisioningStatus::Enrolling, ProvisioningEvent::EnrollmentStarted) => (
                ProvisioningStatus::Enrolling,
                "storage enrollment started".to_owned(),
                None,
                None,
            ),
            (ProvisioningStatus::Enrolling, ProvisioningEvent::Enrolled) => {
                self.completed_at = Some(now);
                self.inbound_email_token_hash = None;
                self.inbound_email_expires_at = Some(now);
                (
                    ProvisioningStatus::Enrolled,
                    "capacity enrolled".to_owned(),
                    None,
                    None,
                )
            }
            (status, ProvisioningEvent::RetryTimer)
                if *status == ProvisioningStatus::FailedRetryable =>
            {
                self.attempt_count = self.attempt_count.saturating_add(1);
                self.retry_after = None;
                (
                    ProvisioningStatus::Starting,
                    "retry timer fired".to_owned(),
                    None,
                    None,
                )
            }
            (
                status,
                ProvisioningEvent::RetryableFailure {
                    code,
                    summary,
                    retry_after,
                },
            ) if status.is_active() && !matches!(status, ProvisioningStatus::Enrolled) => {
                self.last_error_code = Some(code);
                self.last_error_summary = Some(summary);
                self.retry_after = retry_after;
                (
                    ProvisioningStatus::FailedRetryable,
                    "retryable provisioning failure".to_owned(),
                    None,
                    retry_after,
                )
            }
            (status, ProvisioningEvent::PermanentFailure { code, summary })
                if status.is_active() && !matches!(status, ProvisioningStatus::Enrolled) =>
            {
                self.last_error_code = Some(code);
                self.last_error_summary = Some(summary);
                self.retry_after = None;
                self.inbound_email_expires_at = Some(now);
                (
                    ProvisioningStatus::FailedPermanent,
                    "permanent provisioning failure".to_owned(),
                    None,
                    None,
                )
            }
            (status, ProvisioningEvent::NeedsOperator { action, summary })
                if status.is_active() && !matches!(status, ProvisioningStatus::Enrolled) =>
            {
                self.operator_action = Some(action);
                self.last_error_summary = Some(summary);
                (
                    ProvisioningStatus::NeedsOperator,
                    "operator action required".to_owned(),
                    None,
                    None,
                )
            }
            (status, ProvisioningEvent::Cancelled { summary })
                if status.is_active() && !matches!(status, ProvisioningStatus::Enrolled) =>
            {
                self.last_error_summary = Some(summary);
                self.inbound_email_expires_at = Some(now);
                (
                    ProvisioningStatus::Cancelled,
                    "provisioning job cancelled".to_owned(),
                    None,
                    None,
                )
            }
            (status, ProvisioningEvent::Expired) if status.is_active() && !status.is_terminal() => {
                self.last_error_code = Some("PROVISIONING_TIMEOUT".to_owned());
                self.last_error_summary = Some("provisioning job expired".to_owned());
                self.inbound_email_expires_at = Some(now);
                (
                    ProvisioningStatus::FailedPermanent,
                    "provisioning job expired".to_owned(),
                    None,
                    None,
                )
            }
            _ => {
                return Err(ProvisioningError::InvalidTransition {
                    from: from.to_string(),
                    event: event_type,
                });
            }
        };
        self.status = to;
        self.updated_at = now;
        Ok(ProvisioningTransition {
            from,
            to,
            event_type,
            candidate,
            safe_summary,
            retry_after,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ProvisioningEvent {
    Start,
    RegistrationStarted {
        inbound_email_address: String,
        inbound_email_expires_at: DateTime<Utc>,
        inbound_email_token_hash: String,
    },
    AwaitingEmail,
    EmailReceived {
        message_id: String,
    },
    ProviderReady,
    CandidateReady {
        candidate: CapacityCandidate,
    },
    ValidationStarted,
    ValidationPassed,
    EnrollmentStarted,
    Enrolled,
    VerificationCodeReceived {
        token: String,
    },
    VerificationLinkReceived {
        reference: String,
    },
    RetryableFailure {
        code: String,
        summary: String,
        retry_after: Option<DateTime<Utc>>,
    },
    PermanentFailure {
        code: String,
        summary: String,
    },
    NeedsOperator {
        action: String,
        summary: String,
    },
    OperatorCompleted {
        candidate: CapacityCandidate,
    },
    RetryTimer,
    Cancelled {
        summary: String,
    },
    Expired,
}

impl ProvisioningEvent {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Start => "START",
            Self::RegistrationStarted { .. } => "REGISTRATION_STARTED",
            Self::AwaitingEmail => "WAITING_FOR_EMAIL",
            Self::EmailReceived { .. } => "EMAIL_RECEIVED",
            Self::ProviderReady => "PROVIDER_READY",
            Self::CandidateReady { .. } => "CANDIDATE_READY",
            Self::ValidationStarted => "VALIDATING",
            Self::ValidationPassed => "VALIDATION_PASSED",
            Self::EnrollmentStarted => "ENROLLING",
            Self::Enrolled => "ENROLLED",
            Self::VerificationCodeReceived { .. } => "VERIFICATION_CODE_RECEIVED",
            Self::VerificationLinkReceived { .. } => "VERIFICATION_LINK_RECEIVED",
            Self::RetryableFailure { .. } => "FAILED_RETRYABLE",
            Self::PermanentFailure { .. } => "FAILED_PERMANENT",
            Self::NeedsOperator { .. } => "NEEDS_OPERATOR",
            Self::OperatorCompleted { .. } => "OPERATOR_COMPLETED",
            Self::RetryTimer => "RETRY_TIMER",
            Self::Cancelled { .. } => "CANCELLED",
            Self::Expired => "EXPIRED",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProvisioningTransition {
    pub from: ProvisioningStatus,
    pub to: ProvisioningStatus,
    pub event_type: String,
    pub candidate: Option<CapacityCandidate>,
    pub safe_summary: String,
    pub retry_after: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProvisionerCapabilities {
    pub provider_type: String,
    pub pool_types_supported: Vec<String>,
    pub mode: ProvisioningMode,
    pub automatic: bool,
    pub requires_email: bool,
    pub requires_operator: bool,
    pub supports_capacity_query: bool,
    pub supports_reauthentication: bool,
}

#[async_trait]
pub trait CapacityProvisioner: Send + Sync {
    fn capabilities(&self) -> ProvisionerCapabilities;
    async fn start(
        &self,
        request: ProvisionRequest,
    ) -> Result<ProvisionerResult, ProvisioningError>;
    async fn continue_job(
        &self,
        job: &ProvisioningJob,
        event: ProvisioningEmailEvent,
    ) -> Result<ProvisionerResult, ProvisioningError>;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ProvisionerResult {
    WaitingForEmail,
    WaitingForProvider,
    CandidateReady {
        candidate: CapacityCandidate,
    },
    NeedsOperator {
        action: String,
    },
    RetryableFailure {
        code: String,
        summary: String,
        retry_after: Option<DateTime<Utc>>,
    },
    PermanentFailure {
        code: String,
        summary: String,
    },
}

#[derive(Clone)]
struct ProvisionerBinding {
    provisioner: Arc<dyn CapacityProvisioner>,
    validator: Option<Arc<dyn CapacityCandidateValidator>>,
    enroller: Option<Arc<dyn CapacityCandidateEnroller>>,
}

#[derive(Clone, Default)]
pub struct ProvisionerRegistry {
    bindings: Arc<HashMap<String, ProvisionerBinding>>,
}

impl ProvisionerRegistry {
    pub fn register(
        &mut self,
        provider_type: impl Into<String>,
        provisioner: Arc<dyn CapacityProvisioner>,
    ) {
        let mut bindings = (*self.bindings).clone();
        bindings.insert(
            provider_type.into(),
            ProvisionerBinding {
                provisioner,
                validator: None,
                enroller: None,
            },
        );
        self.bindings = Arc::new(bindings);
    }

    pub fn register_with_validation(
        &mut self,
        provider_type: impl Into<String>,
        provisioner: Arc<dyn CapacityProvisioner>,
        validator: Arc<dyn CapacityCandidateValidator>,
        enroller: Arc<dyn CapacityCandidateEnroller>,
    ) {
        let mut bindings = (*self.bindings).clone();
        bindings.insert(
            provider_type.into(),
            ProvisionerBinding {
                provisioner,
                validator: Some(validator),
                enroller: Some(enroller),
            },
        );
        self.bindings = Arc::new(bindings);
    }

    pub fn register_fake(
        &mut self,
        provider_type: impl Into<String>,
        secret_store: Arc<dyn SecretStore>,
    ) {
        self.register_with_validation(
            provider_type,
            Arc::new(FakeAutomaticProvisioner {
                secret_store: secret_store.clone(),
            }),
            Arc::new(FakeCandidateValidator { secret_store }),
            Arc::new(FakeCandidateEnroller),
        );
    }

    fn get(&self, provider_type: &str) -> Option<ProvisionerBinding> {
        self.bindings.get(provider_type).cloned()
    }

    pub fn capabilities(&self) -> Vec<ProvisionerCapabilities> {
        let mut values = self
            .bindings
            .values()
            .map(|binding| binding.provisioner.capabilities())
            .collect::<Vec<_>>();
        values.sort_by(|left, right| left.provider_type.cmp(&right.provider_type));
        values
    }
}

#[async_trait]
pub trait CapacityCandidateValidator: Send + Sync {
    async fn validate(
        &self,
        candidate: &CapacityCandidate,
        requested_capacity_bytes: u64,
    ) -> Result<ValidatedCapacity, ProvisioningError>;
}

#[async_trait]
pub trait CapacityCandidateEnroller: Send + Sync {
    async fn enroll(
        &self,
        pool_id: &str,
        validated: &ValidatedCapacity,
    ) -> Result<(), ProvisioningError>;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ValidatedCapacity {
    pub candidate: CapacityCandidate,
    pub observed_capacity_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProvisioningMailRecord {
    pub message_id: String,
    pub body_sha256: String,
    pub envelope_from: Option<String>,
    pub envelope_to: String,
    pub from_header: Option<String>,
    pub subject: Option<String>,
    pub job_id: Uuid,
}

#[async_trait]
pub trait ProvisioningStore: Send + Sync {
    async fn create_or_get_job(
        &self,
        request: ProvisionRequest,
    ) -> Result<ProvisioningJob, ProvisioningError>;
    async fn claim_start(&self, id: Uuid) -> Result<Option<ProvisioningJob>, ProvisioningError>;
    async fn get_job(&self, id: Uuid) -> Result<Option<ProvisioningJob>, ProvisioningError>;
    async fn apply_event(
        &self,
        id: Uuid,
        idempotency_key: &str,
        event: ProvisioningEvent,
    ) -> Result<ProvisioningJob, ProvisioningError>;
    async fn find_active_job_by_email(
        &self,
        address: &str,
    ) -> Result<Option<ProvisioningJob>, ProvisioningError>;
    async fn claim_mail_nonce(
        &self,
        nonce: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<bool, ProvisioningError>;
    async fn record_mail(&self, record: ProvisioningMailRecord) -> Result<bool, ProvisioningError>;
    async fn list_jobs(
        &self,
        status: Option<ProvisioningStatus>,
        limit: u32,
    ) -> Result<Vec<ProvisioningJob>, ProvisioningError>;
}

#[derive(Default)]
struct InMemoryProvisioningState {
    jobs: HashMap<Uuid, ProvisioningJob>,
    event_keys: HashSet<(Uuid, String)>,
    mail_messages: HashSet<String>,
    mail_nonces: HashMap<String, DateTime<Utc>>,
}

#[derive(Clone, Default)]
pub struct InMemoryProvisioningStore {
    state: Arc<Mutex<InMemoryProvisioningState>>,
}

#[async_trait]
impl ProvisioningStore for InMemoryProvisioningStore {
    async fn create_or_get_job(
        &self,
        request: ProvisionRequest,
    ) -> Result<ProvisioningJob, ProvisioningError> {
        let mut state = self.state.lock().await;
        if let Some(existing) = state.jobs.values().find(|job| {
            job.idempotency_key == request.idempotency_key
                || (job.provider_type == request.provider_type
                    && job.pool_id == request.pool_id
                    && job.status.is_active())
        }) {
            return Ok(existing.clone());
        }
        let job = ProvisioningJob::new(request);
        state.jobs.insert(job.id, job.clone());
        Ok(job)
    }

    async fn claim_start(&self, id: Uuid) -> Result<Option<ProvisioningJob>, ProvisioningError> {
        let mut state = self.state.lock().await;
        let Some(job) = state.jobs.get_mut(&id) else {
            return Err(ProvisioningError::NotFound);
        };
        if job.status != ProvisioningStatus::Created {
            return Ok(None);
        }
        job.apply_event(ProvisioningEvent::Start)?;
        let updated = job.clone();
        state.event_keys.insert((id, "job-start".to_owned()));
        Ok(Some(updated))
    }

    async fn get_job(&self, id: Uuid) -> Result<Option<ProvisioningJob>, ProvisioningError> {
        Ok(self.state.lock().await.jobs.get(&id).cloned())
    }

    async fn apply_event(
        &self,
        id: Uuid,
        idempotency_key: &str,
        event: ProvisioningEvent,
    ) -> Result<ProvisioningJob, ProvisioningError> {
        let mut state = self.state.lock().await;
        if state.event_keys.contains(&(id, idempotency_key.to_owned())) {
            return state
                .jobs
                .get(&id)
                .cloned()
                .ok_or(ProvisioningError::NotFound);
        }
        let updated = {
            let job = state.jobs.get_mut(&id).ok_or(ProvisioningError::NotFound)?;
            job.apply_event(event)?;
            job.clone()
        };
        state.event_keys.insert((id, idempotency_key.to_owned()));
        Ok(updated)
    }

    async fn find_active_job_by_email(
        &self,
        address: &str,
    ) -> Result<Option<ProvisioningJob>, ProvisioningError> {
        let now = Utc::now();
        Ok(self
            .state
            .lock()
            .await
            .jobs
            .values()
            .find(|job| {
                job.status.is_active()
                    && job.inbound_email_address.as_deref() == Some(address)
                    && job
                        .inbound_email_expires_at
                        .is_some_and(|expires| expires > now)
            })
            .cloned())
    }

    async fn claim_mail_nonce(
        &self,
        nonce: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<bool, ProvisioningError> {
        let mut state = self.state.lock().await;
        let now = Utc::now();
        state.mail_nonces.retain(|_, expires| *expires > now);
        if state.mail_nonces.contains_key(nonce) {
            return Ok(false);
        }
        state.mail_nonces.insert(nonce.to_owned(), expires_at);
        Ok(true)
    }

    async fn record_mail(&self, record: ProvisioningMailRecord) -> Result<bool, ProvisioningError> {
        let mut state = self.state.lock().await;
        Ok(state.mail_messages.insert(record.message_id))
    }

    async fn list_jobs(
        &self,
        status: Option<ProvisioningStatus>,
        limit: u32,
    ) -> Result<Vec<ProvisioningJob>, ProvisioningError> {
        let mut jobs = self
            .state
            .lock()
            .await
            .jobs
            .values()
            .filter(|job| status.is_none_or(|expected| job.status == expected))
            .cloned()
            .collect::<Vec<_>>();
        jobs.sort_by_key(|job| job.created_at);
        jobs.truncate(limit.clamp(1, 500) as usize);
        Ok(jobs)
    }
}

#[derive(Clone)]
pub struct ProvisioningManager {
    store: Arc<dyn ProvisioningStore>,
    registry: ProvisionerRegistry,
    email_domain: String,
    alias_ttl: Duration,
}

impl ProvisioningManager {
    pub fn new(store: Arc<dyn ProvisioningStore>, registry: ProvisionerRegistry) -> Self {
        Self {
            store,
            registry,
            email_domain: "vaultnode.pp.ua".to_owned(),
            alias_ttl: Duration::hours(1),
        }
    }

    pub fn with_email_config(mut self, domain: impl Into<String>, ttl: Duration) -> Self {
        self.email_domain = domain.into();
        self.alias_ttl = ttl;
        self
    }

    pub fn with_validator(
        mut self,
        provider_type: &str,
        validator: Arc<dyn CapacityCandidateValidator>,
        enroller: Arc<dyn CapacityCandidateEnroller>,
    ) -> Self {
        let mut bindings = (*self.registry.bindings).clone();
        if let Some(binding) = bindings.get_mut(provider_type) {
            binding.validator = Some(validator);
            binding.enroller = Some(enroller);
        }
        self.registry.bindings = Arc::new(bindings);
        self
    }

    pub fn capabilities(&self) -> Vec<ProvisionerCapabilities> {
        self.registry.capabilities()
    }

    pub async fn ensure_capacity(
        &self,
        request: ProvisionRequest,
    ) -> Result<ProvisioningJob, ProvisioningError> {
        let job = self.store.create_or_get_job(request).await?;
        if job.status != ProvisioningStatus::Created {
            return Ok(job);
        }
        if let Some(job) = self.store.claim_start(job.id).await? {
            return self.start_job(job).await;
        }
        self.store
            .get_job(job.id)
            .await?
            .ok_or(ProvisioningError::NotFound)
    }

    pub async fn retry_job(&self, job_id: Uuid) -> Result<ProvisioningJob, ProvisioningError> {
        let job = self
            .store
            .get_job(job_id)
            .await?
            .ok_or(ProvisioningError::NotFound)?;
        if job.status != ProvisioningStatus::FailedRetryable {
            return Err(ProvisioningError::Conflict(
                "only retryable provisioning jobs can be retried".to_owned(),
            ));
        }
        let job = self
            .store
            .apply_event(
                job.id,
                &format!("operator-retry-{}", job.attempt_count.saturating_add(1)),
                ProvisioningEvent::RetryTimer,
            )
            .await?;
        self.start_job(job).await
    }

    pub async fn cancel_job(
        &self,
        job_id: Uuid,
        summary: impl Into<String>,
    ) -> Result<ProvisioningJob, ProvisioningError> {
        let job = self
            .store
            .get_job(job_id)
            .await?
            .ok_or(ProvisioningError::NotFound)?;
        if job.status.is_terminal() {
            return Ok(job);
        }
        self.store
            .apply_event(
                job_id,
                "operator-cancel",
                ProvisioningEvent::Cancelled {
                    summary: summary.into(),
                },
            )
            .await
    }

    pub async fn expire_job(&self, job_id: Uuid) -> Result<ProvisioningJob, ProvisioningError> {
        let job = self
            .store
            .get_job(job_id)
            .await?
            .ok_or(ProvisioningError::NotFound)?;
        if job.status.is_terminal() {
            return Ok(job);
        }
        self.store
            .apply_event(job_id, "provisioning-expiry", ProvisioningEvent::Expired)
            .await
    }

    pub async fn complete_manual(
        &self,
        job_id: Uuid,
        candidate: CapacityCandidate,
    ) -> Result<ProvisioningJob, ProvisioningError> {
        let job = self
            .store
            .get_job(job_id)
            .await?
            .ok_or(ProvisioningError::NotFound)?;
        if job.status != ProvisioningStatus::NeedsOperator {
            return Err(ProvisioningError::Conflict(
                "only jobs waiting for an operator can be completed manually".to_owned(),
            ));
        }
        let job = self
            .store
            .apply_event(
                job.id,
                "operator-complete",
                ProvisioningEvent::OperatorCompleted { candidate },
            )
            .await?;
        self.validate_and_enroll(job).await
    }

    async fn start_job(&self, job: ProvisioningJob) -> Result<ProvisioningJob, ProvisioningError> {
        let Some(binding) = self.registry.get(&job.provider_type) else {
            return self
                .store
                .apply_event(
                    job.id,
                    "missing-provisioner",
                    ProvisioningEvent::PermanentFailure {
                        code: "PROVISIONER_NOT_CONFIGURED".to_owned(),
                        summary: "no provisioner is configured for this provider".to_owned(),
                    },
                )
                .await;
        };
        let result = match binding.provisioner.start(job.request()).await {
            Ok(result) => result,
            Err(_error) => {
                return self
                    .store
                    .apply_event(
                        job.id,
                        &format!("provisioner-start-failed-{}", job.attempt_count),
                        ProvisioningEvent::RetryableFailure {
                            code: "PROVISIONER_START_FAILED".to_owned(),
                            summary: "provider start failed".to_owned(),
                            retry_after: Some(Utc::now() + Duration::minutes(5)),
                        },
                    )
                    .await;
            }
        };
        match result {
            ProvisionerResult::WaitingForEmail => {
                let alias = generate_email_alias(&self.email_domain, self.alias_ttl)?;
                let token_hash = hash_token(alias.local_part.as_bytes());
                let job = self
                    .store
                    .apply_event(
                        job.id,
                        &format!("registration-started-{}", job.attempt_count),
                        ProvisioningEvent::RegistrationStarted {
                            inbound_email_address: alias.address,
                            inbound_email_expires_at: alias.expires_at,
                            inbound_email_token_hash: token_hash,
                        },
                    )
                    .await?;
                self.store
                    .apply_event(
                        job.id,
                        &format!("waiting-for-email-{}", job.attempt_count),
                        ProvisioningEvent::AwaitingEmail,
                    )
                    .await
            }
            ProvisionerResult::WaitingForProvider => {
                self.store
                    .apply_event(
                        job.id,
                        &format!("waiting-for-provider-{}", job.attempt_count),
                        ProvisioningEvent::ProviderReady,
                    )
                    .await
            }
            result => self.apply_result(job, result).await,
        }
    }

    async fn apply_result(
        &self,
        job: ProvisioningJob,
        result: ProvisionerResult,
    ) -> Result<ProvisioningJob, ProvisioningError> {
        match result {
            ProvisionerResult::WaitingForEmail => Ok(job),
            ProvisionerResult::WaitingForProvider => {
                self.store
                    .apply_event(
                        job.id,
                        &format!("provider-ready-{}", job.attempt_count),
                        ProvisioningEvent::ProviderReady,
                    )
                    .await
            }
            ProvisionerResult::CandidateReady { candidate } => {
                let job = self
                    .store
                    .apply_event(
                        job.id,
                        &format!("candidate-ready-{}", job.attempt_count),
                        ProvisioningEvent::CandidateReady { candidate },
                    )
                    .await?;
                self.validate_and_enroll(job).await
            }
            ProvisionerResult::NeedsOperator { action } => {
                self.store
                    .apply_event(
                        job.id,
                        "needs-operator",
                        ProvisioningEvent::NeedsOperator {
                            action,
                            summary: "operator action is required before validation".to_owned(),
                        },
                    )
                    .await
            }
            ProvisionerResult::RetryableFailure {
                code,
                summary: _summary,
                retry_after,
            } => {
                self.store
                    .apply_event(
                        job.id,
                        "provisioner-failed-retryable",
                        ProvisioningEvent::RetryableFailure {
                            code: safe_provider_code(&code, "PROVIDER_RETRYABLE_FAILURE"),
                            summary: "provider reported a retryable failure".to_owned(),
                            retry_after,
                        },
                    )
                    .await
            }
            ProvisionerResult::PermanentFailure {
                code,
                summary: _summary,
            } => {
                self.store
                    .apply_event(
                        job.id,
                        "provisioner-failed-permanent",
                        ProvisioningEvent::PermanentFailure {
                            code: safe_provider_code(&code, "PROVIDER_PERMANENT_FAILURE"),
                            summary: "provider reported a permanent failure".to_owned(),
                        },
                    )
                    .await
            }
        }
    }

    pub async fn handle_email(
        &self,
        job_id: Uuid,
        message_id: &str,
        event: ProvisioningEmailEvent,
    ) -> Result<ProvisioningJob, ProvisioningError> {
        let job = self
            .store
            .get_job(job_id)
            .await?
            .ok_or(ProvisioningError::NotFound)?;
        self.handle_email_with_record(
            ProvisioningMailRecord {
                message_id: message_id.to_owned(),
                body_sha256: "legacy-wrapper".to_owned(),
                envelope_from: None,
                envelope_to: job.inbound_email_address.clone().unwrap_or_default(),
                from_header: None,
                subject: None,
                job_id,
            },
            event,
        )
        .await
    }

    pub async fn handle_email_with_record(
        &self,
        record: ProvisioningMailRecord,
        event: ProvisioningEmailEvent,
    ) -> Result<ProvisioningJob, ProvisioningError> {
        self.handle_email_with_record_status(record, event)
            .await
            .map(|(job, _duplicate)| job)
    }

    pub async fn handle_email_with_record_status(
        &self,
        record: ProvisioningMailRecord,
        event: ProvisioningEmailEvent,
    ) -> Result<(ProvisioningJob, bool), ProvisioningError> {
        let job = self
            .store
            .get_job(record.job_id)
            .await?
            .ok_or(ProvisioningError::NotFound)?;
        if job.status.is_terminal() {
            return Ok((job, false));
        }
        let accepted = self.store.record_mail(record.clone()).await?;
        if !accepted {
            return Ok((job, true));
        }
        let job = if job.status == ProvisioningStatus::WaitingForEmail {
            self.store
                .apply_event(
                    record.job_id,
                    &format!("mail-received-{}", record.message_id),
                    ProvisioningEvent::EmailReceived {
                        message_id: record.message_id.clone(),
                    },
                )
                .await?
        } else {
            job
        };
        let Some(binding) = self.registry.get(&job.provider_type) else {
            return self
                .store
                .apply_event(
                    job.id,
                    "missing-email-provisioner",
                    ProvisioningEvent::PermanentFailure {
                        code: "PROVISIONER_NOT_CONFIGURED".to_owned(),
                        summary: "no provisioner is configured for this provider".to_owned(),
                    },
                )
                .await
                .map(|job| (job, false));
        };
        let result = match binding.provisioner.continue_job(&job, event).await {
            Ok(result) => result,
            Err(_error) => {
                return self
                    .store
                    .apply_event(
                        job.id,
                        "provisioner-email-failed",
                        ProvisioningEvent::RetryableFailure {
                            code: "PROVIDER_EMAIL_PROCESSING_FAILED".to_owned(),
                            summary: "provider email processing failed".to_owned(),
                            retry_after: Some(Utc::now() + Duration::minutes(5)),
                        },
                    )
                    .await
                    .map(|job| (job, false));
            }
        };
        Ok((self.apply_result(job, result).await?, false))
    }

    async fn validate_and_enroll(
        &self,
        job: ProvisioningJob,
    ) -> Result<ProvisioningJob, ProvisioningError> {
        let binding = self.registry.get(&job.provider_type).ok_or_else(|| {
            ProvisioningError::Configuration(format!("no provisioner for {}", job.provider_type))
        })?;
        let validator = binding.validator.ok_or_else(|| {
            ProvisioningError::Configuration(format!(
                "no authoritative validator is configured for {}",
                job.provider_type
            ))
        })?;
        let enroller = binding.enroller.ok_or_else(|| {
            ProvisioningError::Configuration(format!(
                "no authoritative enroller is configured for {}",
                job.provider_type
            ))
        })?;
        let attempt = job.attempt_count;
        let job = self
            .store
            .apply_event(
                job.id,
                &format!("validation-started-{attempt}"),
                ProvisioningEvent::ValidationStarted,
            )
            .await?;
        let candidate = job.candidate()?;
        let validated = match validator
            .validate(&candidate, job.requested_capacity_bytes)
            .await
        {
            Ok(validated) => validated,
            Err(_error) => {
                return self
                    .store
                    .apply_event(
                        job.id,
                        &format!("validation-failed-{attempt}"),
                        ProvisioningEvent::RetryableFailure {
                            code: "VALIDATION_FAILED".to_owned(),
                            summary: "capacity candidate validation failed".to_owned(),
                            retry_after: Some(Utc::now() + Duration::minutes(5)),
                        },
                    )
                    .await;
            }
        };
        let job = self
            .store
            .apply_event(
                job.id,
                &format!("validation-passed-{attempt}"),
                ProvisioningEvent::ValidationPassed,
            )
            .await?;
        let job = self
            .store
            .apply_event(
                job.id,
                &format!("enrollment-started-{attempt}"),
                ProvisioningEvent::EnrollmentStarted,
            )
            .await?;
        if let Err(_error) = enroller.enroll(&job.pool_id, &validated).await {
            return self
                .store
                .apply_event(
                    job.id,
                    &format!("enrollment-failed-{attempt}"),
                    ProvisioningEvent::RetryableFailure {
                        code: "ENROLLMENT_FAILED".to_owned(),
                        summary: "capacity candidate enrollment failed".to_owned(),
                        retry_after: Some(Utc::now() + Duration::minutes(5)),
                    },
                )
                .await;
        }
        self.store
            .apply_event(
                job.id,
                &format!("enrolled-{attempt}"),
                ProvisioningEvent::Enrolled,
            )
            .await
    }
}

#[derive(Debug, Clone)]
struct EmailAlias {
    local_part: String,
    address: String,
    expires_at: DateTime<Utc>,
}

fn generate_email_alias(domain: &str, ttl: Duration) -> Result<EmailAlias, ProvisioningError> {
    let domain = domain.trim().to_ascii_lowercase();
    if domain.is_empty() || domain.contains('@') || domain.contains(char::is_whitespace) {
        return Err(ProvisioningError::Configuration(
            "invalid provisioning email domain".to_owned(),
        ));
    }
    let mut random = [0_u8; 16];
    OsRng.fill_bytes(&mut random);
    let local_part = format!("p-{}", hex_string(&random));
    Ok(EmailAlias {
        address: format!("{local_part}@{domain}"),
        local_part,
        expires_at: Utc::now() + ttl,
    })
}

fn hex_string(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn hash_token(value: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(value);
    hex_string(&digest.finalize())
}

fn safe_provider_code(value: &str, fallback: &str) -> String {
    let value = value.trim();
    if !value.is_empty()
        && value.len() <= 64
        && value.bytes().all(|byte| {
            byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'-'
        })
    {
        value.to_owned()
    } else {
        fallback.to_owned()
    }
}

struct FakeAutomaticProvisioner {
    secret_store: Arc<dyn SecretStore>,
}

#[async_trait]
impl CapacityProvisioner for FakeAutomaticProvisioner {
    fn capabilities(&self) -> ProvisionerCapabilities {
        ProvisionerCapabilities {
            provider_type: "fake".to_owned(),
            pool_types_supported: vec!["COLD".to_owned()],
            mode: ProvisioningMode::Automatic,
            automatic: true,
            requires_email: true,
            requires_operator: false,
            supports_capacity_query: true,
            supports_reauthentication: true,
        }
    }

    async fn start(
        &self,
        _request: ProvisionRequest,
    ) -> Result<ProvisionerResult, ProvisioningError> {
        Ok(ProvisionerResult::WaitingForEmail)
    }

    async fn continue_job(
        &self,
        job: &ProvisioningJob,
        event: ProvisioningEmailEvent,
    ) -> Result<ProvisionerResult, ProvisioningError> {
        let token = match event {
            ProvisioningEmailEvent::VerificationCodeReceived { token } => token,
            ProvisioningEmailEvent::VerificationLinkReceived { .. }
            | ProvisioningEmailEvent::ProviderReady => {
                return Ok(ProvisionerResult::PermanentFailure {
                    code: "UNSUPPORTED_VERIFICATION_EVENT".to_owned(),
                    summary: "fake provisioner expected a verification code".to_owned(),
                });
            }
        };
        if token != "fake-token" {
            return Ok(ProvisionerResult::PermanentFailure {
                code: "INVALID_VERIFICATION".to_owned(),
                summary: "provider verification did not match the expected fake event".to_owned(),
            });
        }
        let credential_reference = self
            .secret_store
            .put(SecretMaterial::new(b"fake-session".to_vec()))
            .await?;
        Ok(ProvisionerResult::CandidateReady {
            candidate: CapacityCandidate {
                provider_type: job.provider_type.clone(),
                external_account_id: format!("fake-account-{}", job.id.simple()),
                credential_reference,
                expected_capacity_bytes: job.requested_capacity_bytes.max(1024),
                metadata: BTreeMap::from([(String::from("source"), String::from("fake"))]),
            },
        })
    }
}

struct FakeCandidateValidator {
    secret_store: Arc<dyn SecretStore>,
}

#[async_trait]
impl CapacityCandidateValidator for FakeCandidateValidator {
    async fn validate(
        &self,
        candidate: &CapacityCandidate,
        requested_capacity_bytes: u64,
    ) -> Result<ValidatedCapacity, ProvisioningError> {
        let _secret = self
            .secret_store
            .resolve(&candidate.credential_reference)
            .await?;
        if candidate.expected_capacity_bytes < requested_capacity_bytes {
            return Err(ProvisioningError::Provider(
                "fake capacity is too small".to_owned(),
            ));
        }
        let smoke = b"fake-random-smoke";
        let digest = blake3::hash(smoke);
        let downloaded = smoke.to_vec();
        if blake3::hash(&downloaded) != digest {
            return Err(ProvisioningError::Provider(
                "fake BLAKE3 smoke verification failed".to_owned(),
            ));
        }
        Ok(ValidatedCapacity {
            candidate: candidate.clone(),
            observed_capacity_bytes: candidate.expected_capacity_bytes,
        })
    }
}

struct FakeCandidateEnroller;

#[async_trait]
impl CapacityCandidateEnroller for FakeCandidateEnroller {
    async fn enroll(
        &self,
        _pool_id: &str,
        _validated: &ValidatedCapacity,
    ) -> Result<(), ProvisioningError> {
        Ok(())
    }
}

#[derive(Clone)]
struct ManualMegaProvisioner;

#[async_trait]
impl CapacityProvisioner for ManualMegaProvisioner {
    fn capabilities(&self) -> ProvisionerCapabilities {
        ProvisionerCapabilities {
            provider_type: "mega".to_owned(),
            pool_types_supported: vec!["COLD".to_owned()],
            mode: ProvisioningMode::Manual,
            automatic: false,
            requires_email: false,
            requires_operator: true,
            supports_capacity_query: true,
            supports_reauthentication: true,
        }
    }

    async fn start(
        &self,
        _request: ProvisionRequest,
    ) -> Result<ProvisionerResult, ProvisioningError> {
        Ok(ProvisionerResult::NeedsOperator {
            action: "enroll a MEGA account with launcher-admin storage accounts add, validate it, then run provisioning complete-manual".to_owned(),
        })
    }

    async fn continue_job(
        &self,
        _job: &ProvisioningJob,
        _event: ProvisioningEmailEvent,
    ) -> Result<ProvisionerResult, ProvisioningError> {
        Ok(ProvisionerResult::PermanentFailure {
            code: "EMAIL_NOT_SUPPORTED".to_owned(),
            summary: "manual MEGA provisioning does not accept email events".to_owned(),
        })
    }
}

pub fn manual_mega_provisioner() -> Arc<dyn CapacityProvisioner> {
    Arc::new(ManualMegaProvisioner)
}
