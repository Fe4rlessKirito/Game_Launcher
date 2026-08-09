use launcher_common::{Manifest, ManifestValidationError};

pub fn validate_json(bytes: &[u8]) -> Result<Manifest, String> {
    let manifest: Manifest = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    manifest
        .validate()
        .map_err(|error: ManifestValidationError| error.to_string())?;
    Ok(manifest)
}

pub fn content_digest(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_is_stable() {
        assert_eq!(
            content_digest(b"launcher"),
            "3b7acb37a9585954a9b990be24366d560059a5e90ad50b65ba049195c0726a3e"
        );
    }
}
