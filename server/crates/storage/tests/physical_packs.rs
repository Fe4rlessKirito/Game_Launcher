use async_trait::async_trait;
use launcher_storage::{
    DownloadLocation, LocalStorage, StorageClass, StorageError, StorageProvider, StorageRegistry,
    StorageTier,
};
use std::sync::Arc;

struct FakeColdProvider;

#[async_trait]
impl StorageProvider for FakeColdProvider {
    fn provider_id(&self) -> &str {
        "fake-telegram"
    }
    fn tier(&self) -> StorageTier {
        StorageClass::Cold
    }
    fn pool_id(&self) -> &str {
        "fake-telegram"
    }
    fn provider_type(&self) -> &str {
        "telegram"
    }
    fn failure_domain(&self) -> &str {
        "telegram"
    }
    async fn put_encoded(&self, _: &str, _: &[u8]) -> Result<(), StorageError> {
        Ok(())
    }
    async fn read_encoded(&self, _: &str) -> Result<Vec<u8>, StorageError> {
        Err(StorageError::Unavailable("fake COLD".to_owned()))
    }
    async fn delete_encoded(&self, _: &str) -> Result<(), StorageError> {
        Ok(())
    }
    async fn download_location(&self, _: &str) -> Result<DownloadLocation, StorageError> {
        Err(StorageError::Provider("COLD is private".to_owned()))
    }
}

#[tokio::test]
async fn physical_pack_locations_are_direct_hot_only() {
    let root = std::env::temp_dir().join(format!(
        "launcher-pack-provider-test-{}",
        std::process::id()
    ));
    let hot = LocalStorage::new(&root, "https://hot.example");
    let bytes = b"synthetic immutable pack";
    let pack_hash = blake3::hash(bytes).to_hex().to_string();
    hot.put_pack(&pack_hash, bytes).await.unwrap();
    let registry =
        StorageRegistry::new(vec![Arc::new(hot.clone()), Arc::new(FakeColdProvider)]).unwrap();
    let locations = registry
        .download_pack_locations_for_tier(&pack_hash, StorageClass::Hot)
        .await
        .unwrap();
    assert_eq!(locations.len(), 1);
    assert_eq!(
        locations[0].1.url,
        format!("https://hot.example/packs/{pack_hash}")
    );
    assert!(
        registry
            .download_pack_locations_for_tier(&pack_hash, StorageClass::Cold)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(hot.read_pack(&pack_hash).await.unwrap(), bytes);
    let _ = tokio::fs::remove_dir_all(root).await;
}
