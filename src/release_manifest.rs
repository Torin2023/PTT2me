use std::fs::File;
use std::io::Read;
use std::path::Path;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
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
    let text = std::str::from_utf8(&bytes).map_err(|_| ReleaseManifestError::InvalidPublicKey)?;
    let line = text.strip_suffix('\n').unwrap_or(text);
    let line = line.strip_suffix('\r').unwrap_or(line);
    if line.is_empty() || line.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return Err(ReleaseManifestError::InvalidPublicKey);
    }
    STANDARD
        .decode(line)
        .ok()
        .and_then(|decoded| decoded.try_into().ok())
        .ok_or(ReleaseManifestError::InvalidPublicKey)
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
