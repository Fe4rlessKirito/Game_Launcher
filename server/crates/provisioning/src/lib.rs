mod domain;
mod email;
mod secrets;

pub use domain::{
    CapacityCandidate, CapacityCandidateEnroller, CapacityCandidateValidator, CapacityProvisioner,
    InMemoryProvisioningStore, ProvisionRequest, ProvisionerCapabilities, ProvisionerRegistry,
    ProvisionerResult, ProvisioningError, ProvisioningEvent, ProvisioningJob,
    ProvisioningMailRecord, ProvisioningManager, ProvisioningMode, ProvisioningStatus,
    ProvisioningStore, ProvisioningTransition, SecretRef, ValidatedCapacity,
    manual_mega_provisioner,
};
pub use email::{
    EmailIngestHeaders, EmailIngestVerification, FakeProvisioningEmailParser,
    MegaProvisioningEmailParser, ParsedMail, ProvisioningEmailEvent, ProvisioningEmailParser,
    ProvisioningEmailParserRegistry, canonical_email_payload, compute_email_hmac, parse_mime,
    sha256_hex, verify_email_ingest,
};
pub use secrets::{FileSecretStore, MemorySecretStore, Redacted, SecretMaterial, SecretStore};

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use chrono::{Duration, Utc};
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use uuid::Uuid;

    struct FakeValidator;

    #[async_trait]
    impl CapacityCandidateValidator for FakeValidator {
        async fn validate(
            &self,
            candidate: &CapacityCandidate,
            requested_capacity_bytes: u64,
        ) -> Result<ValidatedCapacity, ProvisioningError> {
            if candidate.expected_capacity_bytes < requested_capacity_bytes {
                return Err(ProvisioningError::Provider(
                    "fake capacity is too small".to_owned(),
                ));
            }
            let payload = b"fake-provisioning-smoke";
            let digest = blake3::hash(payload).to_hex().to_string();
            if digest != blake3::hash(payload).to_hex().to_string() {
                return Err(ProvisioningError::Provider(
                    "fake smoke integrity failed".to_owned(),
                ));
            }
            Ok(ValidatedCapacity {
                candidate: candidate.clone(),
                observed_capacity_bytes: candidate.expected_capacity_bytes,
            })
        }
    }

    struct FakeEnroller;

    #[async_trait]
    impl CapacityCandidateEnroller for FakeEnroller {
        async fn enroll(
            &self,
            _pool_id: &str,
            _validated: &ValidatedCapacity,
        ) -> Result<(), ProvisioningError> {
            Ok(())
        }
    }

    struct FailingStartProvisioner;

    #[async_trait]
    impl CapacityProvisioner for FailingStartProvisioner {
        fn capabilities(&self) -> ProvisionerCapabilities {
            ProvisionerCapabilities {
                provider_type: "failing".to_owned(),
                pool_types_supported: vec!["COLD".to_owned()],
                mode: ProvisioningMode::Automatic,
                automatic: true,
                requires_email: false,
                requires_operator: false,
                supports_capacity_query: true,
                supports_reauthentication: false,
            }
        }

        async fn start(
            &self,
            _request: ProvisionRequest,
        ) -> Result<ProvisionerResult, ProvisioningError> {
            Err(ProvisioningError::Provider(
                "injected start failure".to_owned(),
            ))
        }

        async fn continue_job(
            &self,
            _job: &ProvisioningJob,
            _event: ProvisioningEmailEvent,
        ) -> Result<ProvisionerResult, ProvisioningError> {
            Err(ProvisioningError::Provider(
                "injected continuation failure".to_owned(),
            ))
        }
    }

    #[tokio::test]
    async fn fake_provisioning_flow_is_durable_and_idempotent_in_memory() {
        let store = Arc::new(InMemoryProvisioningStore::default());
        let secret_store = Arc::new(MemorySecretStore::default());
        let mut registry = ProvisionerRegistry::default();
        registry.register_fake("fake", secret_store.clone());
        let manager = ProvisioningManager::new(store.clone(), registry).with_validator(
            "fake",
            Arc::new(FakeValidator),
            Arc::new(FakeEnroller),
        );
        let request = ProvisionRequest {
            provider_type: "fake".to_owned(),
            pool_id: "fake-cold".to_owned(),
            requested_capacity_bytes: 1024,
            expires_at: Utc::now() + Duration::hours(1),
            idempotency_key: "full-pool-publication".to_owned(),
        };
        let job = manager.ensure_capacity(request).await.unwrap();
        assert_eq!(job.status, ProvisioningStatus::WaitingForEmail);
        let alias = job.inbound_email_address.clone().unwrap();
        assert!(alias.ends_with("@vaultnode.pp.ua"));

        let parsed = parse_mime(
            b"Message-ID: <fake-1@example.test>\r\nTo: p@example.test\r\nSubject: Fake verification\r\nContent-Type: text/plain\r\n\r\nFAKE-PROVISION-TOKEN: fake-token\r\n",
            4096,
        )
        .unwrap();
        let event = FakeProvisioningEmailParser.parse(&parsed).unwrap();
        let final_job = manager
            .handle_email(job.id, "fake-message-1", event)
            .await
            .unwrap();
        assert_eq!(final_job.status, ProvisioningStatus::Enrolled);
        assert!(final_job.inbound_email_expires_at.unwrap() <= Utc::now());

        let duplicate = manager
            .handle_email(
                job.id,
                "fake-message-1",
                ProvisioningEmailEvent::VerificationCodeReceived {
                    token: "fake-token".to_owned(),
                },
            )
            .await
            .unwrap();
        assert_eq!(duplicate.status, ProvisioningStatus::Enrolled);
        let restarted = store.get_job(job.id).await.unwrap().unwrap();
        let encoded = serde_json::to_vec(&restarted).unwrap();
        let restored: ProvisioningJob = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(restored.status, ProvisioningStatus::Enrolled);
    }

    #[tokio::test]
    async fn concurrent_capacity_requests_create_one_active_job() {
        let store = Arc::new(InMemoryProvisioningStore::default());
        let secret_store = Arc::new(MemorySecretStore::default());
        let mut registry = ProvisionerRegistry::default();
        registry.register_fake("fake", secret_store);
        let manager = Arc::new(ProvisioningManager::new(store, registry));
        let mut tasks = Vec::new();
        for _ in 0..100 {
            let manager = manager.clone();
            tasks.push(tokio::spawn(async move {
                manager
                    .ensure_capacity(ProvisionRequest {
                        provider_type: "fake".to_owned(),
                        pool_id: "fake-cold".to_owned(),
                        requested_capacity_bytes: 1024,
                        expires_at: Utc::now() + Duration::hours(1),
                        idempotency_key: "stampede".to_owned(),
                    })
                    .await
                    .unwrap()
                    .id
            }));
        }
        let mut ids = Vec::new();
        for task in tasks {
            ids.push(task.await.unwrap());
        }
        assert!(ids.windows(2).all(|window| window[0] == window[1]));
    }

    #[tokio::test]
    async fn provider_start_failure_is_retryable_and_uses_a_new_attempt_key() {
        let store = Arc::new(InMemoryProvisioningStore::default());
        let mut registry = ProvisionerRegistry::default();
        registry.register("failing", Arc::new(FailingStartProvisioner));
        let manager = ProvisioningManager::new(store.clone(), registry);
        let job = manager
            .ensure_capacity(ProvisionRequest {
                provider_type: "failing".to_owned(),
                pool_id: "failing-pool".to_owned(),
                requested_capacity_bytes: 1024,
                expires_at: Utc::now() + Duration::hours(1),
                idempotency_key: "failure-injection".to_owned(),
            })
            .await
            .unwrap();
        assert_eq!(job.status, ProvisioningStatus::FailedRetryable);
        let retried = manager.retry_job(job.id).await.unwrap();
        assert_eq!(retried.status, ProvisioningStatus::FailedRetryable);
        assert_eq!(retried.attempt_count, 2);
    }

    #[test]
    fn state_machine_rejects_arbitrary_mutations() {
        let now = Utc::now();
        let mut job = ProvisioningJob::new(ProvisionRequest {
            provider_type: "fake".to_owned(),
            pool_id: "pool".to_owned(),
            requested_capacity_bytes: 1,
            expires_at: now + Duration::hours(1),
            idempotency_key: "state".to_owned(),
        });
        let error = job
            .apply_event_at(ProvisioningEvent::Enrolled, now)
            .unwrap_err();
        assert!(matches!(error, ProvisioningError::InvalidTransition { .. }));
    }

    #[test]
    fn secret_refs_and_debug_wrappers_redact_material() {
        let reference = SecretRef::parse("secret://fake/session").unwrap();
        assert_eq!(reference.as_str(), "secret://fake/session");
        assert_eq!(reference.to_string(), "secret://<redacted>");
        let material = SecretMaterial::new(b"never-log-this".to_vec());
        assert!(!format!("{material:?}").contains("never-log-this"));
        let mut metadata = BTreeMap::new();
        metadata.insert("mode".to_owned(), "test".to_owned());
        assert_eq!(metadata["mode"], "test");
        assert_ne!(Uuid::new_v4(), Uuid::new_v4());
    }

    #[tokio::test]
    async fn file_secret_store_round_trips_and_deletes_without_displaying_material() {
        let root =
            std::env::temp_dir().join(format!("launcher-provisioning-secret-{}", Uuid::new_v4()));
        let store = FileSecretStore::new(&root);
        let material = SecretMaterial::new(b"file-secret-material".to_vec());
        let reference = store.put(material.clone()).await.unwrap();
        assert!(reference.as_str().starts_with("secret://file/"));
        assert!(!format!("{reference:?}").contains("file-secret-material"));
        assert_eq!(store.resolve(&reference).await.unwrap(), material);
        store.delete(&reference).await.unwrap();
        assert!(store.resolve(&reference).await.is_err());
        let _ = tokio::fs::remove_dir_all(root).await;
    }
}
