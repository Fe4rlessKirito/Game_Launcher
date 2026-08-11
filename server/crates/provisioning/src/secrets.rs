use crate::domain::ProvisioningError;
use async_trait::async_trait;
use rand::{RngCore, rngs::OsRng};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct SecretRef(String);

impl SecretRef {
    pub fn parse(value: impl Into<String>) -> Result<Self, ProvisioningError> {
        let value = value.into();
        if !value.starts_with("secret://")
            || value.len() <= "secret://".len()
            || value
                .chars()
                .any(|character| character.is_control() || character.is_whitespace())
        {
            return Err(ProvisioningError::Secret(
                "secret references must use the secret:// scheme and contain no whitespace"
                    .to_owned(),
            ));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretRef(<redacted>)")
    }
}

impl fmt::Display for SecretRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("secret://<redacted>")
    }
}

impl Serialize for SecretRef {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SecretRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct SecretMaterial(Vec<u8>);

impl SecretMaterial {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }
}

impl fmt::Debug for SecretMaterial {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretMaterial(<redacted>)")
    }
}

impl fmt::Display for SecretMaterial {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<redacted>")
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Redacted<T>(pub T);

impl<T> fmt::Debug for Redacted<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<redacted>")
    }
}

impl<T> fmt::Display for Redacted<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<redacted>")
    }
}

#[async_trait]
pub trait SecretStore: Send + Sync {
    async fn put(&self, material: SecretMaterial) -> Result<SecretRef, ProvisioningError>;
    async fn resolve(&self, reference: &SecretRef) -> Result<SecretMaterial, ProvisioningError>;
    async fn delete(&self, reference: &SecretRef) -> Result<(), ProvisioningError>;
}

#[derive(Clone, Default)]
pub struct MemorySecretStore {
    values: Arc<Mutex<HashMap<String, Vec<u8>>>>,
}

#[async_trait]
impl SecretStore for MemorySecretStore {
    async fn put(&self, material: SecretMaterial) -> Result<SecretRef, ProvisioningError> {
        let mut random = [0_u8; 16];
        OsRng.fill_bytes(&mut random);
        let reference = SecretRef::parse(format!(
            "secret://memory/{}",
            random
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        ))?;
        self.values
            .lock()
            .await
            .insert(reference.as_str().to_owned(), material.into_bytes());
        Ok(reference)
    }

    async fn resolve(&self, reference: &SecretRef) -> Result<SecretMaterial, ProvisioningError> {
        self.values
            .lock()
            .await
            .get(reference.as_str())
            .cloned()
            .map(SecretMaterial::new)
            .ok_or_else(|| ProvisioningError::Secret("secret reference is unavailable".to_owned()))
    }

    async fn delete(&self, reference: &SecretRef) -> Result<(), ProvisioningError> {
        self.values.lock().await.remove(reference.as_str());
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct FileSecretStore {
    root: PathBuf,
}

impl FileSecretStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn path_for(&self, reference: &SecretRef) -> Result<PathBuf, ProvisioningError> {
        let value = reference.as_str();
        let name = value.strip_prefix("secret://file/").ok_or_else(|| {
            ProvisioningError::Secret(
                "secret reference does not belong to this file store".to_owned(),
            )
        })?;
        if name.is_empty()
            || name.contains('/')
            || name.contains('\\')
            || name.contains("..")
            || name.chars().any(|character| !character.is_ascii_hexdigit())
        {
            return Err(ProvisioningError::Secret(
                "invalid file secret reference".to_owned(),
            ));
        }
        Ok(self.root.join(format!("{name}.secret")))
    }
}

#[async_trait]
impl SecretStore for FileSecretStore {
    async fn put(&self, material: SecretMaterial) -> Result<SecretRef, ProvisioningError> {
        tokio::fs::create_dir_all(&self.root)
            .await
            .map_err(|error| {
                ProvisioningError::Secret(format!("could not create secret store: {error}"))
            })?;
        let mut random = [0_u8; 16];
        OsRng.fill_bytes(&mut random);
        let name = random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let reference = SecretRef::parse(format!("secret://file/{name}"))?;
        let path = self.path_for(&reference)?;
        tokio::fs::write(path, material.into_bytes())
            .await
            .map_err(|error| {
                ProvisioningError::Secret(format!("could not write secret material: {error}"))
            })?;
        Ok(reference)
    }

    async fn resolve(&self, reference: &SecretRef) -> Result<SecretMaterial, ProvisioningError> {
        let path = self.path_for(reference)?;
        tokio::fs::read(path)
            .await
            .map(SecretMaterial::new)
            .map_err(|_| ProvisioningError::Secret("secret reference is unavailable".to_owned()))
    }

    async fn delete(&self, reference: &SecretRef) -> Result<(), ProvisioningError> {
        let path = self.path_for(reference)?;
        match tokio::fs::remove_file(path).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(ProvisioningError::Secret(format!(
                "could not delete secret material: {error}"
            ))),
        }
    }
}
