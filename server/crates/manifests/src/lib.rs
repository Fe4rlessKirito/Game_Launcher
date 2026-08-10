use base64::{Engine as _, engine::general_purpose::STANDARD};
use launcher_common::{
    MANIFEST_SIGNATURE_SCHEMA_VERSION, Manifest, ManifestSignature, ManifestValidationError,
};
use rand::rngs::OsRng;
use rsa::{
    RsaPrivateKey, RsaPublicKey,
    pkcs1::{DecodeRsaPrivateKey, Error as Pkcs1Error},
    pkcs1v15::Pkcs1v15Sign,
    pkcs8::{DecodePrivateKey, DecodePublicKey, EncodePrivateKey, EncodePublicKey, LineEnding},
};
use sha2::{Digest, Sha256};
use thiserror::Error;

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

#[derive(Debug, Error)]
pub enum SignatureError {
    #[error("RSA key error: {0}")]
    Rsa(#[from] rsa::errors::Error),
    #[error("PEM key error: {0}")]
    Pem(#[from] rsa::pkcs8::Error),
    #[error("PKCS#1 key error: {0}")]
    Pkcs1(#[from] Pkcs1Error),
    #[error("public key encoding error: {0}")]
    PublicKey(#[from] rsa::pkcs8::spki::Error),
    #[error("signature encoding error: {0}")]
    SignatureEncoding(#[from] base64::DecodeError),
    #[error("manifest signature verification failed")]
    Verification,
}

pub fn generate_signing_key() -> Result<RsaPrivateKey, SignatureError> {
    Ok(RsaPrivateKey::new(&mut OsRng, 2048)?)
}

pub fn load_private_key_pem(pem: &str) -> Result<RsaPrivateKey, SignatureError> {
    match RsaPrivateKey::from_pkcs8_pem(pem) {
        Ok(key) => Ok(key),
        Err(_) => Ok(RsaPrivateKey::from_pkcs1_pem(pem)?),
    }
}

pub fn private_key_pem(private_key: &RsaPrivateKey) -> Result<String, SignatureError> {
    Ok(private_key.to_pkcs8_pem(LineEnding::LF)?.to_string())
}

pub fn public_key_pem(private_key: &RsaPrivateKey) -> Result<String, SignatureError> {
    Ok(RsaPublicKey::from(private_key).to_public_key_pem(LineEnding::LF)?)
}

pub fn sign_bytes(
    manifest_bytes: &[u8],
    key_id: impl Into<String>,
    private_key: &RsaPrivateKey,
    include_public_key: bool,
) -> Result<ManifestSignature, SignatureError> {
    let digest = Sha256::digest(manifest_bytes);
    let signature = private_key.sign(Pkcs1v15Sign::new::<Sha256>(), &digest)?;
    let public_key_base64 = include_public_key
        .then(|| RsaPublicKey::from(private_key).to_public_key_der())
        .transpose()?
        .map(|der| STANDARD.encode(der.as_bytes()));
    Ok(ManifestSignature {
        schema_version: MANIFEST_SIGNATURE_SCHEMA_VERSION,
        algorithm: "rsa-sha256-pkcs1-v1_5".to_owned(),
        key_id: key_id.into(),
        manifest_blake3: content_digest(manifest_bytes),
        signature_base64: STANDARD.encode(signature),
        public_key_base64,
    })
}

pub fn verify_bytes(
    manifest_bytes: &[u8],
    signature: &ManifestSignature,
    public_key_der: &[u8],
) -> Result<(), SignatureError> {
    if signature.schema_version != MANIFEST_SIGNATURE_SCHEMA_VERSION
        || signature.algorithm != "rsa-sha256-pkcs1-v1_5"
        || signature.manifest_blake3 != content_digest(manifest_bytes)
    {
        return Err(SignatureError::Verification);
    }
    let public_key = RsaPublicKey::from_public_key_der(public_key_der)
        .map_err(|_| SignatureError::Verification)?;
    let encoded = STANDARD.decode(&signature.signature_base64)?;
    public_key
        .verify(
            Pkcs1v15Sign::new::<Sha256>(),
            &Sha256::digest(manifest_bytes),
            &encoded,
        )
        .map_err(|_| SignatureError::Verification)
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

    #[test]
    fn signatures_round_trip_and_reject_tampering_and_wrong_keys() {
        let private_key = generate_signing_key().unwrap();
        let public_key = RsaPublicKey::from(&private_key)
            .to_public_key_der()
            .unwrap();
        let manifest = br#"{"schema_version":1,"game_id":"game","build_id":"build"}"#;
        let signature = sign_bytes(manifest, "test-key", &private_key, true).unwrap();

        verify_bytes(manifest, &signature, public_key.as_bytes()).unwrap();

        let mut tampered = manifest.to_vec();
        tampered[10] ^= 1;
        assert!(matches!(
            verify_bytes(&tampered, &signature, public_key.as_bytes()),
            Err(SignatureError::Verification)
        ));

        let wrong_key = generate_signing_key().unwrap();
        let wrong_public_key = RsaPublicKey::from(&wrong_key).to_public_key_der().unwrap();
        assert!(matches!(
            verify_bytes(manifest, &signature, wrong_public_key.as_bytes()),
            Err(SignatureError::Verification)
        ));
    }
}
