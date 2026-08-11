use chrono::{Duration as ChronoDuration, Utc};
use launcher_common::{
    ChunkRef, ChunkingConfig, EncodingConfig, FileRecipe, GameSummary, LaunchProfile, Manifest,
    ManifestSignature,
};
use launcher_database::Database;
use launcher_provisioning::{
    ProvisionRequest, ProvisioningEvent, ProvisioningMailRecord, ProvisioningStatus,
    ProvisioningStore,
};
use launcher_storage::{
    CapacityReservationStore, MegaAccountConfig, StorageAccountStatus, StorageError, StoragePolicy,
    StorageTier,
};
use postgresql_embedded::{PostgreSQL, SettingsBuilder};
use std::time::Duration;

fn fixture_manifest() -> Manifest {
    let raw = b"hello launcher";
    let raw_hash = blake3::hash(raw).to_hex().to_string();
    let encoded_hash = blake3::hash(raw).to_hex().to_string();
    Manifest {
        schema_version: 1,
        manifest_id: "manifest-build-1".to_owned(),
        game_id: "game-1".to_owned(),
        build_id: "build-1".to_owned(),
        display_version: "1.0.0".to_owned(),
        generated_at: Utc::now(),
        chunking: ChunkingConfig::default(),
        encoding: EncodingConfig::default(),
        files: vec![FileRecipe {
            path: "game.exe".to_owned(),
            size: raw.len() as u64,
            blake3: raw_hash.clone(),
            chunks: vec![ChunkRef {
                raw_hash,
                raw_size: raw.len() as u64,
                encoded_hash: encoded_hash.clone(),
                encoded_size: raw.len() as u64,
                object_key: format!("chunks/encoded/{encoded_hash}.bin"),
            }],
        }],
        launch: LaunchProfile {
            executable: "game.exe".to_owned(),
            working_directory: ".".to_owned(),
            ..LaunchProfile::default()
        },
    }
}

#[tokio::test]
async fn postgres_repository_migrates_publishes_and_recovers_leases()
-> Result<(), Box<dyn std::error::Error>> {
    let settings = SettingsBuilder::new()
        .timeout(Some(Duration::from_secs(60)))
        .build();
    let mut postgres = PostgreSQL::new(settings);
    postgres.setup().await?;
    postgres.start().await?;
    let database_name = format!("launcher_test_{}", std::process::id());
    postgres.create_database(&database_name).await?;

    let database = Database::connect(&postgres.settings().url(&database_name)).await?;
    // Build a database at the pre-pool schema first, seed legacy locations,
    // then run the real forward migration. This protects the upgrade path from
    // silently assuming a freshly-created database.
    sqlx::raw_sql(include_str!("../../../../migrations/001_initial.sql"))
        .execute(database.pool())
        .await?;
    sqlx::raw_sql(include_str!(
        "../../../../migrations/002_storage_tiering.sql"
    ))
    .execute(database.pool())
    .await?;
    let legacy_hot_hash = "a".repeat(64);
    let legacy_cold_hash = "b".repeat(64);
    for hash in [&legacy_hot_hash, &legacy_cold_hash] {
        sqlx::query(
            "INSERT INTO chunks(encoded_hash, encoded_size, encoding_id) VALUES($1,1,'legacy')",
        )
        .bind(hash)
        .execute(database.pool())
        .await?;
    }
    sqlx::query(
        "INSERT INTO storage_objects(encoded_hash, encoded_size, provider, tier, object_key, verified_at)
         VALUES($1,1,'legacy-s3','HOT','chunks/encoded/a.bin',now())",
    )
    .bind(&legacy_hot_hash)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO storage_locations(encoded_hash, provider, tier, object_key, direct_url, priority, verified_at)
         VALUES($1,'legacy-mega','COLD','chunks/encoded/b.bin','',0,now())",
    )
    .bind(&legacy_cold_hash)
    .execute(database.pool())
    .await?;
    database.migrate().await?;
    assert!(database.schema_status().await?.ready());
    let provisioning_job = database
        .create_or_get_job(ProvisionRequest {
            provider_type: "mega".to_owned(),
            pool_id: "legacy-mega".to_owned(),
            requested_capacity_bytes: 1024,
            expires_at: Utc::now() + ChronoDuration::hours(1),
            idempotency_key: "postgres-provisioning-upgrade".to_owned(),
        })
        .await?;
    assert_eq!(provisioning_job.status, ProvisioningStatus::Created);
    assert_eq!(
        database
            .create_or_get_job(ProvisionRequest {
                provider_type: "mega".to_owned(),
                pool_id: "legacy-mega".to_owned(),
                requested_capacity_bytes: 1024,
                expires_at: Utc::now() + ChronoDuration::hours(1),
                idempotency_key: "postgres-provisioning-upgrade".to_owned(),
            })
            .await?
            .id,
        provisioning_job.id
    );
    let provisioning_job = database
        .apply_event(
            provisioning_job.id,
            "postgres-start",
            ProvisioningEvent::Start,
        )
        .await?;
    let alias = "p-upgrade@vaultnode.pp.ua".to_owned();
    let provisioning_job = database
        .apply_event(
            provisioning_job.id,
            "postgres-registration",
            ProvisioningEvent::RegistrationStarted {
                inbound_email_address: alias.clone(),
                inbound_email_expires_at: Utc::now() + ChronoDuration::minutes(10),
                inbound_email_token_hash: "hashed-alias".to_owned(),
            },
        )
        .await?;
    let provisioning_job = database
        .apply_event(
            provisioning_job.id,
            "postgres-waiting-email",
            ProvisioningEvent::AwaitingEmail,
        )
        .await?;
    assert_eq!(
        database.find_active_job_by_email(&alias).await?.unwrap().id,
        provisioning_job.id
    );
    let provisioning_job = database
        .apply_event(
            provisioning_job.id,
            "postgres-mail-received",
            ProvisioningEvent::EmailReceived {
                message_id: "<upgrade-mail@example.test>".to_owned(),
            },
        )
        .await?;
    assert_eq!(provisioning_job.status, ProvisioningStatus::EmailReceived);
    assert_eq!(
        database
            .apply_event(
                provisioning_job.id,
                "postgres-mail-received",
                ProvisioningEvent::EmailReceived {
                    message_id: "<upgrade-mail@example.test>".to_owned(),
                },
            )
            .await?
            .status,
        ProvisioningStatus::EmailReceived
    );
    assert!(
        database
            .claim_mail_nonce("upgrade-nonce", Utc::now() + ChronoDuration::minutes(5))
            .await?
    );
    assert!(
        !database
            .claim_mail_nonce("upgrade-nonce", Utc::now() + ChronoDuration::minutes(5))
            .await?
    );
    let mail = ProvisioningMailRecord {
        message_id: "<upgrade-mail@example.test>".to_owned(),
        body_sha256: "a".repeat(64),
        envelope_from: Some("sender@example.test".to_owned()),
        envelope_to: alias,
        from_header: Some("sender@example.test".to_owned()),
        subject: Some("fixture".to_owned()),
        job_id: provisioning_job.id,
    };
    assert!(database.record_mail(mail.clone()).await?);
    assert!(!database.record_mail(mail).await?);
    let migrated_pools: Vec<(String, String, String)> =
        sqlx::query_as("SELECT id, storage_class, failure_domain FROM storage_pools ORDER BY id")
            .fetch_all(database.pool())
            .await?;
    assert!(migrated_pools.iter().any(|(id, class, domain)| {
        id == "legacy-s3" && class == "HOT" && domain == "legacy-s3"
    }));
    assert!(
        migrated_pools.iter().any(|(id, class, domain)| {
            id == "legacy-mega" && class == "COLD" && domain == "mega"
        })
    );
    let migrated_location: (String, String) = sqlx::query_as(
        "SELECT pool_id, failure_domain FROM storage_locations WHERE encoded_hash=$1",
    )
    .bind(&legacy_cold_hash)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        migrated_location,
        ("legacy-mega".to_owned(), "mega".to_owned())
    );

    let game = GameSummary {
        id: "game-1".to_owned(),
        slug: "game-1".to_owned(),
        title: "Test Game".to_owned(),
        description: "Disposable integration fixture".to_owned(),
        hero_image_url: None,
        cover_image_url: None,
        latest_build: None,
    };
    database.upsert_game(&game).await?;

    let manifest = fixture_manifest();
    let signature = ManifestSignature {
        schema_version: 1,
        algorithm: "test".to_owned(),
        key_id: "test-key".to_owned(),
        manifest_blake3: "0".repeat(64),
        signature_base64: "AQ==".to_owned(),
        public_key_base64: None,
    };
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
    database
        .upsert_build_with_bytes(&manifest, &manifest_bytes, Some(&signature), "READY")
        .await?;
    let chunk = &manifest.files[0].chunks[0];
    database
        .add_chunk(
            &chunk.encoded_hash,
            chunk.encoded_size as i64,
            &manifest.encoding.id,
        )
        .await?;
    database
        .attach_build_chunk(
            &manifest.build_id,
            &chunk.encoded_hash,
            chunk.raw_size as i64,
            &chunk.raw_hash,
            0,
        )
        .await?;
    assert!(database.publish_build(&manifest.build_id).await.is_err());
    database
        .add_storage_location(
            &chunk.encoded_hash,
            "local",
            &chunk.object_key,
            "http://mirror.invalid/chunk",
            0,
        )
        .await?;
    let locations = database
        .get_storage_locations(std::slice::from_ref(&chunk.encoded_hash))
        .await?;
    assert_eq!(locations[&chunk.encoded_hash][0].provider, "local");
    database
        .publish_build_with_storage_policy(&manifest.build_id, &StoragePolicy::default())
        .await?;

    assert_eq!(
        database.get_manifest(&manifest.build_id).await?,
        Some(manifest.clone())
    );
    assert_eq!(
        database.get_manifest_bytes(&manifest.build_id).await?,
        Some(manifest_bytes)
    );
    assert_eq!(
        database.get_signature(&manifest.build_id).await?,
        Some(signature)
    );
    assert_eq!(database.get_game("game-1").await?.unwrap().id, "game-1");

    sqlx::query("INSERT INTO ingestion_jobs(build_id, stage) VALUES($1, 'QUEUED')")
        .bind(&manifest.build_id)
        .execute(database.pool())
        .await?;
    let (claim_a, claim_b) = tokio::join!(
        database.claim_job("integration-worker-a", 30),
        database.claim_job("integration-worker-b", 30)
    );
    let claim_a = claim_a?;
    let claim_b = claim_b?;
    assert_eq!(claim_a.is_some() as u8 + claim_b.is_some() as u8, 1);
    let claimed = claim_a.or(claim_b).unwrap();
    assert_eq!(claimed.attempts, 1);
    database.fail_job(claimed.id, "transient", true).await?;
    let reclaimed = database
        .claim_job("integration-worker-2", 30)
        .await?
        .unwrap();
    assert_eq!(reclaimed.id, claimed.id);
    database.complete_job(reclaimed.id, "DONE").await?;
    database.complete_job(reclaimed.id, "DONE").await?;
    assert!(
        database
            .claim_job("integration-worker-3", 30)
            .await?
            .is_none()
    );

    let max_attempt_job = sqlx::query(
        "INSERT INTO ingestion_jobs(build_id, stage, max_attempts) VALUES($1, 'QUEUED', 1) RETURNING id",
    )
    .bind(&manifest.build_id)
    .fetch_one(database.pool())
    .await?;
    let max_attempt_id: i64 = sqlx::Row::try_get(&max_attempt_job, "id")?;
    let max_attempt_claim = database.claim_job("one-shot-worker", 30).await?.unwrap();
    assert_eq!(max_attempt_claim.id, max_attempt_id);
    sqlx::query(
        "UPDATE ingestion_jobs SET lease_until = now() - interval '1 second' WHERE id = $1",
    )
    .bind(max_attempt_id)
    .execute(database.pool())
    .await?;
    assert_eq!(database.recover_expired_jobs().await?, 1);
    assert!(
        database
            .claim_job("replacement-worker", 30)
            .await?
            .is_none()
    );

    database.close().await;
    postgres.drop_database(&database_name).await?;
    postgres.stop().await?;
    Ok(())
}

#[tokio::test]
async fn postgres_capacity_reservations_are_atomic_and_recoverable()
-> Result<(), Box<dyn std::error::Error>> {
    let settings = SettingsBuilder::new()
        .timeout(Some(Duration::from_secs(60)))
        .build();
    let mut postgres = PostgreSQL::new(settings);
    postgres.setup().await?;
    postgres.start().await?;
    let database_name = format!("launcher_capacity_test_{}", std::process::id());
    postgres.create_database(&database_name).await?;
    let database = Database::connect(&postgres.settings().url(&database_name)).await?;
    database.migrate().await?;
    database
        .upsert_storage_provider(
            "mega-cold",
            "mega",
            StorageTier::Cold,
            serde_json::json!({"test":true}),
        )
        .await?;
    let account = MegaAccountConfig {
        account_id: "mega-a".to_owned(),
        credential_reference: "test://session".to_owned(),
        command_dir: Default::default(),
        home_dir: "test-home".into(),
        remote_root: "/launcher".to_owned(),
        capacity_bytes: 10,
        safety_margin_bytes: 0,
        timeout_seconds: 1,
        max_output_bytes: 1024,
    };
    database
        .upsert_storage_account("mega-cold", &account, StorageAccountStatus::Active)
        .await?;
    let first_hash = "a".repeat(64);
    let second_hash = "b".repeat(64);
    for hash in [&first_hash, &second_hash] {
        database.add_chunk(hash, 10, "test").await?;
    }
    let first = database.reserve("mega-a", &first_hash, 10, Duration::from_secs(60));
    let second = database.reserve("mega-a", &second_hash, 1, Duration::from_secs(60));
    let (first, second) = tokio::join!(first, second);
    let first = first?;
    assert!(matches!(second, Err(StorageError::NeedsCapacity { .. })));
    database.release(&first.reservation_id).await?;
    let recovered = database
        .reserve("mega-a", &second_hash, 1, Duration::from_millis(1))
        .await?;
    tokio::time::sleep(Duration::from_millis(5)).await;
    assert_eq!(database.recover_expired().await?, 1);
    assert!(
        database
            .reserve("mega-a", &first_hash, 10, Duration::from_secs(60))
            .await
            .is_ok()
    );
    database.release(&recovered.reservation_id).await?;
    database.close().await;
    postgres.drop_database(&database_name).await?;
    postgres.stop().await?;
    Ok(())
}
