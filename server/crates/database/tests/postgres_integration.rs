use chrono::Utc;
use launcher_common::{
    ChunkRef, ChunkingConfig, EncodingConfig, FileRecipe, GameSummary, LaunchProfile, Manifest,
    ManifestSignature,
};
use launcher_database::Database;
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
    database.migrate().await?;

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
    database.publish_build(&manifest.build_id).await?;

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
