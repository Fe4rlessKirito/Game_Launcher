use super::{StorageClass, StoragePolicy};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PhysicalPackHealth {
    Healthy,
    UnderReplicated,
    Degraded,
    Lost,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PackReplicaObservation {
    pub provider: String,
    pub pool_id: String,
    pub failure_domain: String,
    pub storage_class: StorageClass,
    pub verified: bool,
    pub readable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PhysicalPackReconciliation {
    pub pack_hash: String,
    pub health: PhysicalPackHealth,
    pub verified_hot_replicas: u32,
    pub verified_cold_replicas: u32,
    pub verified_archive_replicas: u32,
    pub verified_hot_failure_domains: u32,
    pub verified_cold_failure_domains: u32,
    pub verified_archive_failure_domains: u32,
    pub repair_classes: Vec<StorageClass>,
    pub scrub_required: bool,
}

pub struct PhysicalPackReconciler;

impl PhysicalPackReconciler {
    pub fn evaluate(
        pack_hash: impl Into<String>,
        observations: &[PackReplicaObservation],
        policy: &StoragePolicy,
    ) -> PhysicalPackReconciliation {
        let verified = observations
            .iter()
            .filter(|observation| observation.verified)
            .collect::<Vec<_>>();
        let count = |class: StorageClass| {
            verified
                .iter()
                .filter(|observation| observation.storage_class == class)
                .count() as u32
        };
        let domains = |class: StorageClass| {
            verified
                .iter()
                .filter(|observation| observation.storage_class == class)
                .map(|observation| observation.failure_domain.as_str())
                .collect::<HashSet<_>>()
                .len() as u32
        };
        let counts = [
            (
                StorageClass::Hot,
                count(StorageClass::Hot),
                domains(StorageClass::Hot),
            ),
            (
                StorageClass::Cold,
                count(StorageClass::Cold),
                domains(StorageClass::Cold),
            ),
            (
                StorageClass::Archive,
                count(StorageClass::Archive),
                domains(StorageClass::Archive),
            ),
        ];
        let repair_classes = counts
            .iter()
            .filter_map(|(class, replicas, failure_domains)| {
                (*replicas < policy.required_replicas(*class)
                    || *failure_domains < policy.required_failure_domains(*class))
                .then_some(*class)
            })
            .collect::<Vec<_>>();
        let any_verified = !verified.is_empty();
        let scrub_required = observations
            .iter()
            .any(|observation| observation.verified && !observation.readable);
        let health = if !any_verified {
            PhysicalPackHealth::Lost
        } else if !repair_classes.is_empty() {
            PhysicalPackHealth::UnderReplicated
        } else if scrub_required {
            PhysicalPackHealth::Degraded
        } else {
            PhysicalPackHealth::Healthy
        };
        PhysicalPackReconciliation {
            pack_hash: pack_hash.into(),
            health,
            verified_hot_replicas: counts[0].1,
            verified_cold_replicas: counts[1].1,
            verified_archive_replicas: counts[2].1,
            verified_hot_failure_domains: counts[0].2,
            verified_cold_failure_domains: counts[1].2,
            verified_archive_failure_domains: counts[2].2,
            repair_classes,
            scrub_required,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> StoragePolicy {
        StoragePolicy {
            min_verified_hot_replicas: 1,
            min_verified_cold_replicas: 1,
            min_verified_archive_replicas: 0,
            preferred_hot_replicas: 1,
            preferred_cold_replicas: 1,
            preferred_archive_replicas: 0,
            min_hot_failure_domains: 1,
            min_cold_failure_domains: 1,
            min_archive_failure_domains: 0,
            cold_backup_required: true,
            restore_mode: super::super::RestoreMode::OnDemand,
        }
    }

    #[test]
    fn detects_under_replication_and_scrub_damage() {
        let observation = PackReplicaObservation {
            provider: "hot".to_owned(),
            pool_id: "hot".to_owned(),
            failure_domain: "railway".to_owned(),
            storage_class: StorageClass::Hot,
            verified: true,
            readable: false,
        };
        let result = PhysicalPackReconciler::evaluate("a".repeat(64), &[observation], &policy());
        assert_eq!(result.health, PhysicalPackHealth::UnderReplicated);
        assert!(result.repair_classes.contains(&StorageClass::Cold));
        assert!(result.scrub_required);
    }

    #[test]
    fn healthy_requires_verified_class_and_domain_coverage() {
        let observations = vec![
            PackReplicaObservation {
                provider: "hot".to_owned(),
                pool_id: "hot".to_owned(),
                failure_domain: "railway".to_owned(),
                storage_class: StorageClass::Hot,
                verified: true,
                readable: true,
            },
            PackReplicaObservation {
                provider: "cold".to_owned(),
                pool_id: "cold".to_owned(),
                failure_domain: "mega".to_owned(),
                storage_class: StorageClass::Cold,
                verified: true,
                readable: true,
            },
        ];
        let result = PhysicalPackReconciler::evaluate("b".repeat(64), &observations, &policy());
        assert_eq!(result.health, PhysicalPackHealth::Healthy);
        assert!(result.repair_classes.is_empty());
    }
}
