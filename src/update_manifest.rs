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
const MAX_ENVELOPE_BYTES: usize = 64 * 1024;
const MAX_ARTIFACT_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedRelease {
    pub version: Version,
    pub build: u64,
    pub source_commit: String,
    pub minimum_macos: MacOsVersion,
    pub required_model: RequiredModel,
    pub fresh_install: ArtifactDescriptor,
    pub application_update: ArtifactDescriptor,
    pub published_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredModel {
    pub id: String,
    pub manifest_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactDescriptor {
    pub url: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct MacOsVersion {
    major: u64,
    minor: u64,
}

impl MacOsVersion {
    pub fn parse(value: &str) -> Result<Self, ManifestError> {
        let mut components = value.split('.');
        let major = parse_version_component(components.next(), "minimum_macos")?;
        let minor = parse_version_component(components.next(), "minimum_macos")?;
        if components.next().is_some() {
            return Err(ManifestError::InvalidField("minimum_macos"));
        }
        Ok(Self { major, minor })
    }
}

impl std::fmt::Display for MacOsVersion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}.{}", self.major, self.minor)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledBuild {
    pub version: Version,
    pub build: u64,
    pub source_commit: String,
}

impl InstalledBuild {
    pub fn parse(version: &str, build: u64, source_commit: &str) -> Result<Self, ManifestError> {
        let version = parse_stable_version(version)?;
        if build == 0 {
            return Err(ManifestError::InvalidField("build"));
        }
        if !is_lower_hex(source_commit, 40) {
            return Err(ManifestError::InvalidField("source_commit"));
        }
        Ok(Self {
            version,
            build,
            source_commit: source_commit.to_owned(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseDisposition {
    Available,
    Current,
    DivergedLocal,
    UnpublishedLocal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelAvailability {
    Verified(String),
    Missing,
    Invalid,
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
    required_model: RequiredModelPayload,
    fresh_install: ArtifactPayload,
    application_update: ArtifactPayload,
    published_at: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RequiredModelPayload {
    id: String,
    manifest_sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactPayload {
    url: String,
    sha256: String,
    size: u64,
}

pub fn verify_envelope(
    bytes: &[u8],
    public_key: &[u8; 32],
) -> Result<VerifiedRelease, ManifestError> {
    if bytes.len() > MAX_ENVELOPE_BYTES {
        return Err(ManifestError::InvalidEnvelope);
    }
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
            std::cmp::Ordering::Equal => {
                if installed.source_commit == release.source_commit {
                    ReleaseDisposition::Current
                } else {
                    ReleaseDisposition::DivergedLocal
                }
            }
            std::cmp::Ordering::Greater => ReleaseDisposition::UnpublishedLocal,
        },
    }
}

pub fn select_artifact<'a>(
    release: &'a VerifiedRelease,
    model: &ModelAvailability,
) -> &'a ArtifactDescriptor {
    match model {
        ModelAvailability::Verified(id) if id == &release.required_model.id => {
            &release.application_update
        }
        ModelAvailability::Verified(_)
        | ModelAvailability::Missing
        | ModelAvailability::Invalid => &release.fresh_install,
    }
}

pub fn verify_artifact(
    mut reader: impl Read,
    artifact: &ArtifactDescriptor,
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

    if total != artifact.size {
        return Err(ManifestError::ArtifactSizeMismatch);
    }
    let digest = format!("{:x}", hasher.finalize());
    if digest != artifact.sha256 {
        return Err(ManifestError::ArtifactDigestMismatch);
    }
    Ok(())
}

fn validate_payload(payload: ReleasePayload) -> Result<VerifiedRelease, ManifestError> {
    if payload.channel != STABLE_CHANNEL {
        return Err(ManifestError::InvalidField("channel"));
    }
    let version = parse_stable_version(&payload.version)?;
    if payload.build == 0 {
        return Err(ManifestError::InvalidField("build"));
    }
    if !is_lower_hex(&payload.source_commit, 40) {
        return Err(ManifestError::InvalidField("source_commit"));
    }
    let minimum_macos = MacOsVersion::parse(&payload.minimum_macos)?;
    if minimum_macos
        < (MacOsVersion {
            major: 13,
            minor: 0,
        })
    {
        return Err(ManifestError::InvalidField("minimum_macos"));
    }
    if payload.architecture != TARGET_ARCHITECTURE {
        return Err(ManifestError::InvalidField("architecture"));
    }
    if !is_model_id(&payload.required_model.id) {
        return Err(ManifestError::InvalidField("id"));
    }
    if !is_lower_hex(&payload.required_model.manifest_sha256, 64) {
        return Err(ManifestError::InvalidField("manifest_sha256"));
    }
    if !is_utc_timestamp(&payload.published_at) {
        return Err(ManifestError::InvalidField("published_at"));
    }
    let fresh_install = validate_artifact(payload.fresh_install, &version, "full")?;
    let application_update = validate_artifact(payload.application_update, &version, "update")?;

    Ok(VerifiedRelease {
        version,
        build: payload.build,
        source_commit: payload.source_commit,
        minimum_macos,
        required_model: RequiredModel {
            id: payload.required_model.id,
            manifest_sha256: payload.required_model.manifest_sha256,
        },
        fresh_install,
        application_update,
        published_at: payload.published_at,
    })
}

fn validate_artifact(
    artifact: ArtifactPayload,
    version: &Version,
    variant: &str,
) -> Result<ArtifactDescriptor, ManifestError> {
    let expected_url = format!(
        "https://github.com/Torin2023/PTT2me/releases/download/v{version}/PTT2me-{version}-{variant}-macos-arm64.dmg"
    );
    if artifact.url != expected_url {
        return Err(ManifestError::InvalidField("url"));
    }
    if !is_lower_hex(&artifact.sha256, 64) {
        return Err(ManifestError::InvalidField("sha256"));
    }
    if artifact.size == 0 || artifact.size > MAX_ARTIFACT_BYTES {
        return Err(ManifestError::InvalidField("size"));
    }
    Ok(ArtifactDescriptor {
        url: artifact.url,
        sha256: artifact.sha256,
        size: artifact.size,
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

fn parse_version_component(
    component: Option<&str>,
    field: &'static str,
) -> Result<u64, ManifestError> {
    let component = component.ok_or(ManifestError::InvalidField(field))?;
    if component.is_empty()
        || !component.bytes().all(|byte| byte.is_ascii_digit())
        || (component.len() > 1 && component.starts_with('0'))
    {
        return Err(ManifestError::InvalidField(field));
    }
    component
        .parse()
        .map_err(|_| ManifestError::InvalidField(field))
}

fn is_model_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && value.as_bytes().first() != Some(&b'-')
        && value.as_bytes().last() != Some(&b'-')
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
    let Some(year) = parse(0..4) else {
        return false;
    };
    let Some(month @ 1..=12) = parse(5..7) else {
        return false;
    };
    let Some(day) = parse(8..10) else {
        return false;
    };
    let leap_year = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days_in_month = match month {
        2 if leap_year => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    year != 0
        && (1..=days_in_month).contains(&day)
        && matches!(parse(11..13), Some(0..=23))
        && matches!(parse(14..16), Some(0..=59))
        && matches!(parse(17..19), Some(0..=59))
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io::Cursor;

    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    use ed25519_dalek::{Signer, SigningKey};
    use serde_json::json;

    use super::{
        classify_release, select_artifact, verify_artifact, verify_envelope, InstalledBuild,
        MacOsVersion, ManifestError, ModelAvailability, ReleaseDisposition,
    };

    const TEST_PRIVATE_KEY: [u8; 32] = [
        0x11, 0x23, 0x35, 0x47, 0x59, 0x6b, 0x7d, 0x8f, 0x90, 0xa2, 0xb4, 0xc6, 0xd8, 0xea, 0xfc,
        0x0e, 0x10, 0x32, 0x54, 0x76, 0x98, 0xba, 0xdc, 0xfe, 0xef, 0xcd, 0xab, 0x89, 0x67, 0x45,
        0x23, 0x01,
    ];
    pub(crate) const TEST_PUBLIC_KEY: [u8; 32] = [
        0xca, 0xde, 0x19, 0x2a, 0xa1, 0xee, 0x6d, 0xbb, 0x06, 0x00, 0xbb, 0x4a, 0x6d, 0x89, 0xf7,
        0x16, 0x2a, 0xfc, 0x7d, 0x02, 0x7e, 0x85, 0xfb, 0x9a, 0x14, 0xa3, 0xfb, 0x8b, 0xe0, 0xd2,
        0x3f, 0x8a,
    ];
    const FULL_ARTIFACT: &[u8] = b"verified PTT2me full dmg fixture";
    const UPDATE_ARTIFACT: &[u8] = b"verified PTT2me update dmg fixture";
    const SIGNED_PAYLOAD: &[u8] = b"{\"channel\":\"stable\",\"version\":\"1.0.6\",\"build\":202608011200,\"source_commit\":\"0123456789abcdef0123456789abcdef01234567\",\"minimum_macos\":\"13.0\",\"architecture\":\"arm64\",\"required_model\":{\"id\":\"gigaam-v3-rnnt-v1\",\"manifest_sha256\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"},\"fresh_install\":{\"url\":\"https://github.com/Torin2023/PTT2me/releases/download/v1.0.6/PTT2me-1.0.6-full-macos-arm64.dmg\",\"sha256\":\"80530994d8ca7568fcba045b34d82b6f0c31188a07aae38de1fede676e08a1a4\",\"size\":32},\"application_update\":{\"url\":\"https://github.com/Torin2023/PTT2me/releases/download/v1.0.6/PTT2me-1.0.6-update-macos-arm64.dmg\",\"sha256\":\"79a45998882238cd19dfefda21805a21e2769e6750a8bffab9f3443101d2b5f6\",\"size\":34},\"published_at\":\"2026-08-01T12:00:00Z\"}\n";
    pub(crate) const SIGNED_ENVELOPE: &[u8] = br#"{"schema":1,"payload":"eyJjaGFubmVsIjoic3RhYmxlIiwidmVyc2lvbiI6IjEuMC42IiwiYnVpbGQiOjIwMjYwODAxMTIwMCwic291cmNlX2NvbW1pdCI6IjAxMjM0NTY3ODlhYmNkZWYwMTIzNDU2Nzg5YWJjZGVmMDEyMzQ1NjciLCJtaW5pbXVtX21hY29zIjoiMTMuMCIsImFyY2hpdGVjdHVyZSI6ImFybTY0IiwicmVxdWlyZWRfbW9kZWwiOnsiaWQiOiJnaWdhYW0tdjMtcm5udC12MSIsIm1hbmlmZXN0X3NoYTI1NiI6ImFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWEifSwiZnJlc2hfaW5zdGFsbCI6eyJ1cmwiOiJodHRwczovL2dpdGh1Yi5jb20vVG9yaW4yMDIzL1BUVDJtZS9yZWxlYXNlcy9kb3dubG9hZC92MS4wLjYvUFRUMm1lLTEuMC42LWZ1bGwtbWFjb3MtYXJtNjQuZG1nIiwic2hhMjU2IjoiODA1MzA5OTRkOGNhNzU2OGZjYmEwNDViMzRkODJiNmYwYzMxMTg4YTA3YWFlMzhkZTFmZWRlNjc2ZTA4YTFhNCIsInNpemUiOjMyfSwiYXBwbGljYXRpb25fdXBkYXRlIjp7InVybCI6Imh0dHBzOi8vZ2l0aHViLmNvbS9Ub3JpbjIwMjMvUFRUMm1lL3JlbGVhc2VzL2Rvd25sb2FkL3YxLjAuNi9QVFQybWUtMS4wLjYtdXBkYXRlLW1hY29zLWFybTY0LmRtZyIsInNoYTI1NiI6Ijc5YTQ1OTk4ODgyMjM4Y2QxOWRmZWZkYTIxODA1YTIxZTI3NjllNjc1MGE4YmZmYWI5ZjM0NDMxMDFkMmI1ZjYiLCJzaXplIjozNH0sInB1Ymxpc2hlZF9hdCI6IjIwMjYtMDgtMDFUMTI6MDA6MDBaIn0K","signature":"pzYWjkXXLXUNYd6oqqlrmEsbKTpR0v4QAtKjLWO8jpXbrrOzF2xYFlR/Hzi99WMaS8I+aCqV1Ac+yFY9pzj/Cw=="}"#;

    fn sign_payload(payload: &[u8]) -> Vec<u8> {
        let signing_key = SigningKey::from_bytes(&TEST_PRIVATE_KEY);
        let signature = signing_key.sign(payload);
        serde_json::to_vec(&json!({
            "schema": 1,
            "payload": STANDARD.encode(payload),
            "signature": STANDARD.encode(signature.to_bytes())
        }))
        .unwrap()
    }

    fn payload_with(mut change: impl FnMut(&mut serde_json::Value)) -> Vec<u8> {
        let mut payload: serde_json::Value = serde_json::from_slice(SIGNED_PAYLOAD).unwrap();
        change(&mut payload);
        serde_json::to_vec(&payload).unwrap()
    }

    fn valid_release() -> super::VerifiedRelease {
        verify_envelope(SIGNED_ENVELOPE, &TEST_PUBLIC_KEY).unwrap()
    }

    #[test]
    fn literal_signed_fixture_exposes_both_artifacts_and_required_model() {
        let release = valid_release();

        assert_eq!(release.version.to_string(), "1.0.6");
        assert_eq!(release.build, 202608011200);
        assert_eq!(
            release.source_commit,
            "0123456789abcdef0123456789abcdef01234567"
        );
        assert_eq!(release.minimum_macos.to_string(), "13.0");
        assert_eq!(release.required_model.id, "gigaam-v3-rnnt-v1");
        assert_eq!(
            release.required_model.manifest_sha256,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert_eq!(
            release.fresh_install.url,
            "https://github.com/Torin2023/PTT2me/releases/download/v1.0.6/PTT2me-1.0.6-full-macos-arm64.dmg"
        );
        assert_eq!(
            release.fresh_install.sha256,
            "80530994d8ca7568fcba045b34d82b6f0c31188a07aae38de1fede676e08a1a4"
        );
        assert_eq!(release.fresh_install.size, 32);
        assert_eq!(
            release.application_update.url,
            "https://github.com/Torin2023/PTT2me/releases/download/v1.0.6/PTT2me-1.0.6-update-macos-arm64.dmg"
        );
        assert_eq!(
            release.application_update.sha256,
            "79a45998882238cd19dfefda21805a21e2769e6750a8bffab9f3443101d2b5f6"
        );
        assert_eq!(release.application_update.size, 34);
        assert_eq!(release.published_at, "2026-08-01T12:00:00Z");
    }

    #[test]
    fn rejects_literal_fixture_after_payload_or_signature_alteration() {
        let mut altered_payload: serde_json::Value =
            serde_json::from_slice(SIGNED_ENVELOPE).unwrap();
        altered_payload["payload"] = json!(STANDARD.encode(payload_with(|payload| {
            payload["version"] = json!("9.9.9");
        })));
        let altered_payload = serde_json::to_vec(&altered_payload).unwrap();

        assert_eq!(
            verify_envelope(&altered_payload, &TEST_PUBLIC_KEY),
            Err(ManifestError::InvalidSignature)
        );

        let mut altered_signature: serde_json::Value =
            serde_json::from_slice(SIGNED_ENVELOPE).unwrap();
        altered_signature["signature"] = json!("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==");
        let altered_signature = serde_json::to_vec(&altered_signature).unwrap();

        assert_eq!(
            verify_envelope(&altered_signature, &TEST_PUBLIC_KEY),
            Err(ManifestError::InvalidSignature)
        );
    }

    #[test]
    fn rejects_signed_invalid_scalar_fields() {
        let invalid: [(&str, &str, serde_json::Value); 12] = [
            ("channel", "channel", json!("nightly")),
            ("version", "version", json!("1.0.6-beta.1")),
            ("build", "build", json!(0)),
            ("source_commit", "source_commit", json!("ABC")),
            ("minimum_macos", "minimum_macos", json!("12.6")),
            ("minimum_macos", "minimum_macos", json!("013.0")),
            ("architecture", "architecture", json!("x86_64")),
            (
                "published_at",
                "published_at",
                json!("2026-02-29T12:00:00Z"),
            ),
            (
                "published_at",
                "published_at",
                json!("2026-08-01T12:00:60Z"),
            ),
            (
                "published_at",
                "published_at",
                json!("2026-08-01T12:00:00+00:00"),
            ),
            ("required_model", "id", json!("../gigaam")),
            ("required_model", "manifest_sha256", json!("ABCD")),
        ];

        for (container, field, replacement) in invalid {
            let payload = payload_with(|payload| {
                if container == field {
                    payload[field] = replacement.clone();
                } else {
                    payload[container][field] = replacement.clone();
                }
            });
            assert_eq!(
                verify_envelope(&sign_payload(&payload), &TEST_PUBLIC_KEY),
                Err(ManifestError::InvalidField(field)),
                "{container}.{field} must be rejected"
            );
        }
    }

    #[test]
    fn rejects_invalid_artifact_urls_hashes_and_sizes() {
        let invalid = [
            ("fresh_install", "url", json!("http://example.com/full.dmg")),
            ("application_update", "url", json!("https://github.com/Torin2023/PTT2me/releases/download/v1.0.6/PTT2me-1.0.6-full-macos-arm64.dmg")),
            ("fresh_install", "sha256", json!("abcd")),
            ("application_update", "sha256", json!("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")),
            ("fresh_install", "size", json!(0)),
            ("application_update", "size", json!(1_073_741_825_u64)),
        ];

        for (artifact, field, replacement) in invalid {
            let payload = payload_with(|payload| {
                payload[artifact][field] = replacement.clone();
            });
            assert_eq!(
                verify_envelope(&sign_payload(&payload), &TEST_PUBLIC_KEY),
                Err(ManifestError::InvalidField(field)),
                "{artifact}.{field} must be rejected"
            );
        }
    }

    #[test]
    fn bounds_envelope_before_decoding() {
        assert_eq!(
            verify_envelope(&vec![b' '; 65_537], &TEST_PUBLIC_KEY),
            Err(ManifestError::InvalidEnvelope)
        );
    }

    #[test]
    fn compares_macos_versions_numerically() {
        assert!(MacOsVersion::parse("14.10").unwrap() > MacOsVersion::parse("14.9").unwrap());
        assert!(MacOsVersion::parse("13.0").unwrap() < MacOsVersion::parse("14.0").unwrap());
        assert_eq!(MacOsVersion::parse("13.0").unwrap().to_string(), "13.0");
    }

    #[test]
    fn classifies_build_commit_divergence_and_refuses_downgrades() {
        let release = valid_release();

        assert_eq!(
            classify_release(
                &InstalledBuild::parse(
                    "1.0.5",
                    202607310831,
                    "1111111111111111111111111111111111111111"
                )
                .unwrap(),
                &release
            ),
            ReleaseDisposition::Available
        );
        assert_eq!(
            classify_release(
                &InstalledBuild::parse(
                    "1.0.6",
                    202608011200,
                    "0123456789abcdef0123456789abcdef01234567"
                )
                .unwrap(),
                &release
            ),
            ReleaseDisposition::Current
        );
        assert_eq!(
            classify_release(
                &InstalledBuild::parse(
                    "1.0.6",
                    202608011200,
                    "1111111111111111111111111111111111111111"
                )
                .unwrap(),
                &release
            ),
            ReleaseDisposition::DivergedLocal
        );
        assert_eq!(
            classify_release(
                &InstalledBuild::parse(
                    "1.0.6",
                    202608011201,
                    "1111111111111111111111111111111111111111"
                )
                .unwrap(),
                &release
            ),
            ReleaseDisposition::UnpublishedLocal
        );
        assert_eq!(
            classify_release(
                &InstalledBuild::parse("1.1.0", 1, "1111111111111111111111111111111111111111")
                    .unwrap(),
                &release
            ),
            ReleaseDisposition::UnpublishedLocal
        );
    }

    #[test]
    fn installed_build_rejects_zero_build_and_invalid_commit() {
        assert_eq!(
            InstalledBuild::parse("1.0.6", 0, "0123456789abcdef0123456789abcdef01234567"),
            Err(ManifestError::InvalidField("build"))
        );
        assert_eq!(
            InstalledBuild::parse("1.0.6", 1, "not-a-commit"),
            Err(ManifestError::InvalidField("source_commit"))
        );
    }

    #[test]
    fn selects_update_only_for_verified_matching_model_id() {
        let release = valid_release();

        assert_eq!(
            select_artifact(
                &release,
                &ModelAvailability::Verified("gigaam-v3-rnnt-v1".to_owned())
            ),
            &release.application_update
        );
        assert_eq!(
            select_artifact(
                &release,
                &ModelAvailability::Verified("gigaam-v3-rnnt-v2".to_owned())
            ),
            &release.fresh_install
        );
        assert_eq!(
            select_artifact(&release, &ModelAvailability::Missing),
            &release.fresh_install
        );
        assert_eq!(
            select_artifact(&release, &ModelAvailability::Invalid),
            &release.fresh_install
        );
    }

    #[test]
    fn verifies_each_artifact_against_its_own_literal_bytes() {
        let release = valid_release();

        assert_eq!(
            verify_artifact(Cursor::new(FULL_ARTIFACT), &release.fresh_install),
            Ok(())
        );
        assert_eq!(
            verify_artifact(Cursor::new(UPDATE_ARTIFACT), &release.application_update),
            Ok(())
        );
        assert_eq!(
            verify_artifact(Cursor::new(b"wrong bytes"), &release.fresh_install),
            Err(ManifestError::ArtifactSizeMismatch)
        );
        let same_size_wrong_bytes = vec![b'x'; UPDATE_ARTIFACT.len()];
        assert_eq!(
            verify_artifact(
                Cursor::new(same_size_wrong_bytes),
                &release.application_update
            ),
            Err(ManifestError::ArtifactDigestMismatch)
        );
    }
}
