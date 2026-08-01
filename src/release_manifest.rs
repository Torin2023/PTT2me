use std::fs::File;
use std::io::Read;
use std::path::Path;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use ed25519_dalek::VerifyingKey;
use sha2::{Digest, Sha256};

use crate::model_store::MODEL_ID;
use crate::update_manifest::{verify_artifact, verify_envelope, ManifestError, VerifiedRelease};

const KEY_FILE_LIMIT: u64 = 256;
const ENVELOPE_FILE_LIMIT: u64 = 64 * 1024;

#[derive(Debug)]
pub enum ReleaseManifestError {
    Read(&'static str),
    InvalidPublicKey,
    InvalidManifest(ManifestError),
    FullArtifact(ManifestError),
    UpdateArtifact(ManifestError),
    UnexpectedRequiredModel,
    RequiredModelManifestDigestMismatch,
}

impl std::fmt::Display for ReleaseManifestError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read(kind) => write!(formatter, "could not read {kind}"),
            Self::InvalidPublicKey => formatter.write_str(
                "public key must be one base64 line containing exactly 32 raw Ed25519 bytes",
            ),
            Self::InvalidManifest(error) => {
                write!(formatter, "signed update manifest is invalid: {error:?}")
            }
            Self::FullArtifact(error) => {
                write!(formatter, "Full DMG verification failed: {error:?}")
            }
            Self::UpdateArtifact(error) => {
                write!(formatter, "Update DMG verification failed: {error:?}")
            }
            Self::UnexpectedRequiredModel => {
                formatter.write_str("signed release requires an unexpected model id")
            }
            Self::RequiredModelManifestDigestMismatch => {
                formatter.write_str("required model manifest digest mismatch")
            }
        }
    }
}

pub fn verify_release_files(
    public_key_path: &Path,
    manifest_path: &Path,
    full_dmg_path: &Path,
    update_dmg_path: &Path,
    model_manifest_path: &Path,
) -> Result<VerifiedRelease, ReleaseManifestError> {
    let public_key = read_public_key(public_key_path)?;
    let manifest = read_bounded(manifest_path, ENVELOPE_FILE_LIMIT, "signed update manifest")?;
    let release =
        verify_envelope(&manifest, &public_key).map_err(ReleaseManifestError::InvalidManifest)?;
    if release.required_model.id != MODEL_ID {
        return Err(ReleaseManifestError::UnexpectedRequiredModel);
    }

    let full_dmg = File::open(full_dmg_path).map_err(|_| ReleaseManifestError::Read("Full DMG"))?;
    verify_artifact(full_dmg, &release.fresh_install)
        .map_err(ReleaseManifestError::FullArtifact)?;

    let update_dmg =
        File::open(update_dmg_path).map_err(|_| ReleaseManifestError::Read("Update DMG"))?;
    verify_artifact(update_dmg, &release.application_update)
        .map_err(ReleaseManifestError::UpdateArtifact)?;

    let model_digest = digest_file(model_manifest_path)?;
    if model_digest != release.required_model.manifest_sha256 {
        return Err(ReleaseManifestError::RequiredModelManifestDigestMismatch);
    }
    Ok(release)
}

fn read_public_key(path: &Path) -> Result<[u8; 32], ReleaseManifestError> {
    let bytes = read_bounded(path, KEY_FILE_LIMIT, "key file")?;
    parse_public_key(&bytes)
}

pub(crate) fn parse_public_key(bytes: &[u8]) -> Result<[u8; 32], ReleaseManifestError> {
    let line = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    let text = std::str::from_utf8(line).map_err(|_| ReleaseManifestError::InvalidPublicKey)?;
    if text.is_empty() || text.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return Err(ReleaseManifestError::InvalidPublicKey);
    }
    let key: [u8; 32] = STANDARD
        .decode(text)
        .ok()
        .and_then(|decoded| decoded.try_into().ok())
        .ok_or(ReleaseManifestError::InvalidPublicKey)?;
    if STANDARD.encode(key) != text || VerifyingKey::from_bytes(&key).is_err() {
        return Err(ReleaseManifestError::InvalidPublicKey);
    }
    Ok(key)
}

fn read_bounded(
    path: &Path,
    limit: u64,
    kind: &'static str,
) -> Result<Vec<u8>, ReleaseManifestError> {
    let file = File::open(path).map_err(|_| ReleaseManifestError::Read(kind))?;
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ReleaseManifestError::Read(kind))?;
    if bytes.len() as u64 > limit {
        return Err(ReleaseManifestError::Read(kind));
    }
    Ok(bytes)
}

fn digest_file(path: &Path) -> Result<String, ReleaseManifestError> {
    let mut file =
        File::open(path).map_err(|_| ReleaseManifestError::Read("required model manifest"))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| ReleaseManifestError::Read("required model manifest"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::SigningKey;

    use super::*;

    #[test]
    fn public_key_parser_accepts_one_canonical_line() {
        let expected = SigningKey::from_bytes(&[0x42; 32])
            .verifying_key()
            .to_bytes();
        let encoded = format!("{}\n", STANDARD.encode(expected));

        assert_eq!(parse_public_key(encoded.as_bytes()).unwrap(), expected);
    }

    #[test]
    fn public_key_parser_rejects_noncanonical_and_invalid_ed25519_keys() {
        let valid = SigningKey::from_bytes(&[0x42; 32])
            .verifying_key()
            .to_bytes();
        let valid_line = STANDARD.encode(valid);

        assert!(parse_public_key(format!("{valid_line}\r\n").as_bytes()).is_err());
        assert!(parse_public_key(format!("{valid_line}\nextra").as_bytes()).is_err());
        let invalid = (0_u8..=u8::MAX)
            .map(|byte| [byte; 32])
            .find(|candidate| VerifyingKey::from_bytes(candidate).is_err())
            .expect("at least one raw 32-byte value is not an Ed25519 verifying key");
        assert!(parse_public_key(format!("{}\n", STANDARD.encode(invalid)).as_bytes()).is_err());
    }
}
