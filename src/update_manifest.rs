use std::io::Read;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};

const ENVELOPE_SCHEMA: u8 = 1;
const STABLE_CHANNEL: &str = "stable";
const TARGET_ARCHITECTURE: &str = "arm64";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedRelease {
    pub version: Version,
    pub build: u64,
    pub source_commit: String,
    pub minimum_macos: String,
    pub download_url: String,
    pub sha256: String,
    pub size: u64,
    pub published_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledBuild {
    pub version: Version,
    pub build: u64,
}

impl InstalledBuild {
    pub fn parse(version: &str, build: u64) -> Result<Self, ManifestError> {
        let version = parse_stable_version(version)?;
        Ok(Self { version, build })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseDisposition {
    Available,
    Current,
    UnpublishedLocal,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ManifestError {
    InvalidEnvelope,
    UnsupportedSchema,
    InvalidPublicKey,
    InvalidSignature,
    InvalidPayload,
    InvalidField(&'static str),
    ArtifactRead,
    ArtifactSizeMismatch,
    ArtifactDigestMismatch,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    schema: u8,
    payload: String,
    signature: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleasePayload {
    channel: String,
    version: String,
    build: u64,
    source_commit: String,
    minimum_macos: String,
    architecture: String,
    download_url: String,
    sha256: String,
    size: u64,
    published_at: String,
}

pub fn verify_envelope(
    bytes: &[u8],
    public_key: &[u8; 32],
) -> Result<VerifiedRelease, ManifestError> {
    let envelope: Envelope =
        serde_json::from_slice(bytes).map_err(|_| ManifestError::InvalidEnvelope)?;
    if envelope.schema != ENVELOPE_SCHEMA {
        return Err(ManifestError::UnsupportedSchema);
    }

    let payload = STANDARD
        .decode(envelope.payload)
        .map_err(|_| ManifestError::InvalidEnvelope)?;
    let signature_bytes = STANDARD
        .decode(envelope.signature)
        .map_err(|_| ManifestError::InvalidEnvelope)?;
    let signature_bytes: [u8; 64] = signature_bytes
        .try_into()
        .map_err(|_| ManifestError::InvalidEnvelope)?;
    let verifying_key =
        VerifyingKey::from_bytes(public_key).map_err(|_| ManifestError::InvalidPublicKey)?;
    verifying_key
        .verify(&payload, &Signature::from_bytes(&signature_bytes))
        .map_err(|_| ManifestError::InvalidSignature)?;

    let payload: ReleasePayload =
        serde_json::from_slice(&payload).map_err(|_| ManifestError::InvalidPayload)?;
    validate_payload(payload)
}

pub fn classify_release(
    installed: &InstalledBuild,
    release: &VerifiedRelease,
) -> ReleaseDisposition {
    match installed.version.cmp(&release.version) {
        std::cmp::Ordering::Less => ReleaseDisposition::Available,
        std::cmp::Ordering::Greater => ReleaseDisposition::UnpublishedLocal,
        std::cmp::Ordering::Equal => match installed.build.cmp(&release.build) {
            std::cmp::Ordering::Less => ReleaseDisposition::Available,
            std::cmp::Ordering::Equal => ReleaseDisposition::Current,
            std::cmp::Ordering::Greater => ReleaseDisposition::UnpublishedLocal,
        },
    }
}

pub fn verify_artifact(
    mut reader: impl Read,
    release: &VerifiedRelease,
) -> Result<(), ManifestError> {
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|_| ManifestError::ArtifactRead)?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(read as u64)
            .ok_or(ManifestError::ArtifactSizeMismatch)?;
        hasher.update(&buffer[..read]);
    }

    if total != release.size {
        return Err(ManifestError::ArtifactSizeMismatch);
    }
    let digest = format!("{:x}", hasher.finalize());
    if digest != release.sha256 {
        return Err(ManifestError::ArtifactDigestMismatch);
    }
    Ok(())
}

fn validate_payload(payload: ReleasePayload) -> Result<VerifiedRelease, ManifestError> {
    if payload.channel != STABLE_CHANNEL {
        return Err(ManifestError::InvalidField("channel"));
    }
    let version = parse_stable_version(&payload.version)?;
    if !is_lower_hex(&payload.source_commit, 40) {
        return Err(ManifestError::InvalidField("source_commit"));
    }
    if !is_macos_version(&payload.minimum_macos) {
        return Err(ManifestError::InvalidField("minimum_macos"));
    }
    if payload.architecture != TARGET_ARCHITECTURE {
        return Err(ManifestError::InvalidField("architecture"));
    }
    let expected_url = format!(
        "https://github.com/Torin2023/PTT2me/releases/download/v{version}/PTT2me-{version}-macos-arm64.dmg"
    );
    if payload.download_url != expected_url {
        return Err(ManifestError::InvalidField("download_url"));
    }
    if !is_lower_hex(&payload.sha256, 64) {
        return Err(ManifestError::InvalidField("sha256"));
    }
    if payload.size == 0 {
        return Err(ManifestError::InvalidField("size"));
    }
    if !is_utc_timestamp(&payload.published_at) {
        return Err(ManifestError::InvalidField("published_at"));
    }

    Ok(VerifiedRelease {
        version,
        build: payload.build,
        source_commit: payload.source_commit,
        minimum_macos: payload.minimum_macos,
        download_url: payload.download_url,
        sha256: payload.sha256,
        size: payload.size,
        published_at: payload.published_at,
    })
}

fn parse_stable_version(value: &str) -> Result<Version, ManifestError> {
    let version = Version::parse(value).map_err(|_| ManifestError::InvalidField("version"))?;
    if !version.pre.is_empty() || !version.build.is_empty() || version.to_string() != value {
        return Err(ManifestError::InvalidField("version"));
    }
    Ok(version)
}

fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_macos_version(value: &str) -> bool {
    let mut components = value.split('.');
    let Some(major) = components.next() else {
        return false;
    };
    let Some(minor) = components.next() else {
        return false;
    };
    components.next().is_none()
        && !major.is_empty()
        && !minor.is_empty()
        && major.bytes().all(|byte| byte.is_ascii_digit())
        && minor.bytes().all(|byte| byte.is_ascii_digit())
}

fn is_utc_timestamp(value: &str) -> bool {
    if value.len() != 20
        || value.as_bytes()[4] != b'-'
        || value.as_bytes()[7] != b'-'
        || value.as_bytes()[10] != b'T'
        || value.as_bytes()[13] != b':'
        || value.as_bytes()[16] != b':'
        || value.as_bytes()[19] != b'Z'
    {
        return false;
    }
    for index in [0, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15, 17, 18] {
        if !value.as_bytes()[index].is_ascii_digit() {
            return false;
        }
    }
    let parse = |range: std::ops::Range<usize>| value[range].parse::<u32>().ok();
    matches!(parse(5..7), Some(1..=12))
        && matches!(parse(8..10), Some(1..=31))
        && matches!(parse(11..13), Some(0..=23))
        && matches!(parse(14..16), Some(0..=59))
        && matches!(parse(17..19), Some(0..=59))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    use ed25519_dalek::{Signer, SigningKey};
    use serde_json::json;

    use super::{
        classify_release, verify_artifact, verify_envelope, InstalledBuild, ManifestError,
        ReleaseDisposition,
    };

    const PRIVATE_KEY: [u8; 32] = [
        0x11, 0x23, 0x35, 0x47, 0x59, 0x6b, 0x7d, 0x8f, 0x90, 0xa2, 0xb4, 0xc6, 0xd8, 0xea, 0xfc,
        0x0e, 0x10, 0x32, 0x54, 0x76, 0x98, 0xba, 0xdc, 0xfe, 0xef, 0xcd, 0xab, 0x89, 0x67, 0x45,
        0x23, 0x01,
    ];
    const ARTIFACT: &[u8] = b"verified PTT2me dmg fixture";
    const ARTIFACT_SHA256: &str =
        "024e5cfd5ac7dd791c40e312a4abd2f6b351324c0d8b6e6d4d41356e7f072d2a";

    fn payload(overrides: &[(&str, serde_json::Value)]) -> Vec<u8> {
        let mut value = json!({
            "channel": "stable",
            "version": "1.0.6",
            "build": 202608011200_u64,
            "source_commit": "0123456789abcdef0123456789abcdef01234567",
            "minimum_macos": "13.0",
            "architecture": "arm64",
            "download_url": "https://github.com/Torin2023/PTT2me/releases/download/v1.0.6/PTT2me-1.0.6-macos-arm64.dmg",
            "sha256": ARTIFACT_SHA256,
            "size": ARTIFACT.len(),
            "published_at": "2026-08-01T12:00:00Z"
        });
        for (key, replacement) in overrides {
            value[*key] = replacement.clone();
        }
        serde_json::to_vec(&value).unwrap()
    }

    fn envelope(payload: &[u8], signing_key: &SigningKey) -> Vec<u8> {
        let signature = signing_key.sign(payload);
        serde_json::to_vec(&json!({
            "schema": 1,
            "payload": STANDARD.encode(payload),
            "signature": STANDARD.encode(signature.to_bytes())
        }))
        .unwrap()
    }

    fn valid_release() -> (super::VerifiedRelease, [u8; 32]) {
        let signing_key = SigningKey::from_bytes(&PRIVATE_KEY);
        let public_key = signing_key.verifying_key().to_bytes();
        let bytes = envelope(&payload(&[]), &signing_key);
        let release = verify_envelope(&bytes, &public_key).unwrap();
        (release, public_key)
    }

    #[test]
    fn verifies_exact_signed_payload_and_exposes_validated_release() {
        let (release, _) = valid_release();

        assert_eq!(release.version.to_string(), "1.0.6");
        assert_eq!(release.build, 202608011200);
        assert_eq!(release.size, ARTIFACT.len() as u64);
        assert_eq!(release.sha256, ARTIFACT_SHA256);
    }

    #[test]
    fn rejects_payload_changed_after_signing() {
        let signing_key = SigningKey::from_bytes(&PRIVATE_KEY);
        let public_key = signing_key.verifying_key().to_bytes();
        let original = payload(&[]);
        let mut value: serde_json::Value = serde_json::from_slice(&original).unwrap();
        value["version"] = json!("9.9.9");
        let changed_payload = serde_json::to_vec(&value).unwrap();
        let signature = signing_key.sign(&original);
        let bytes = serde_json::to_vec(&json!({
            "schema": 1,
            "payload": STANDARD.encode(changed_payload),
            "signature": STANDARD.encode(signature.to_bytes())
        }))
        .unwrap();

        assert_eq!(
            verify_envelope(&bytes, &public_key),
            Err(ManifestError::InvalidSignature)
        );
    }

    #[test]
    fn rejects_signature_from_another_key() {
        let expected_key = SigningKey::from_bytes(&PRIVATE_KEY);
        let other_key = SigningKey::from_bytes(&[0x5a; 32]);
        let bytes = envelope(&payload(&[]), &other_key);

        assert_eq!(
            verify_envelope(&bytes, &expected_key.verifying_key().to_bytes()),
            Err(ManifestError::InvalidSignature)
        );
    }

    #[test]
    fn rejects_untrusted_release_fields_after_signature_verification() {
        let signing_key = SigningKey::from_bytes(&PRIVATE_KEY);
        let public_key = signing_key.verifying_key().to_bytes();
        let invalid = [
            ("channel", json!("nightly")),
            ("version", json!("not-semver")),
            ("source_commit", json!("ABC")),
            ("minimum_macos", json!("latest")),
            ("architecture", json!("x86_64")),
            ("download_url", json!("http://example.com/update.dmg")),
            ("sha256", json!("abcd")),
            ("size", json!(0)),
            ("published_at", json!("tomorrow")),
        ];

        for (field, replacement) in invalid {
            let bytes = envelope(&payload(&[(field, replacement)]), &signing_key);
            assert_eq!(
                verify_envelope(&bytes, &public_key),
                Err(ManifestError::InvalidField(field)),
                "field {field} must be rejected"
            );
        }
    }

    #[test]
    fn classifies_remote_release_without_downgrading_local_builds() {
        let (release, _) = valid_release();

        assert_eq!(
            classify_release(
                &InstalledBuild::parse("1.0.5", 202607310831).unwrap(),
                &release
            ),
            ReleaseDisposition::Available
        );
        assert_eq!(
            classify_release(
                &InstalledBuild::parse("1.0.6", 202608011200).unwrap(),
                &release
            ),
            ReleaseDisposition::Current
        );
        assert_eq!(
            classify_release(
                &InstalledBuild::parse("1.0.6", 202608011201).unwrap(),
                &release
            ),
            ReleaseDisposition::UnpublishedLocal
        );
        assert_eq!(
            classify_release(&InstalledBuild::parse("1.1.0", 1).unwrap(), &release),
            ReleaseDisposition::UnpublishedLocal
        );
    }

    #[test]
    fn verifies_artifact_size_and_digest() {
        let (release, _) = valid_release();

        assert_eq!(verify_artifact(Cursor::new(ARTIFACT), &release), Ok(()));
        assert_eq!(
            verify_artifact(Cursor::new(b"wrong bytes"), &release),
            Err(ManifestError::ArtifactSizeMismatch)
        );
        let same_size_wrong_bytes = vec![b'x'; ARTIFACT.len()];
        assert_eq!(
            verify_artifact(Cursor::new(same_size_wrong_bytes), &release),
            Err(ManifestError::ArtifactDigestMismatch)
        );
    }
}
