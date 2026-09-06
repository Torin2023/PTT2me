use std::fs::{self, OpenOptions};
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

const MAX_CORPUS_CASES: usize = 16;
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const MAX_LABEL_BYTES: usize = 256;
const MAX_ID_BYTES: usize = 64;
const MAX_AUDIO_FRAMES: usize = ((ptt2me::constants::MAX_CAPTURE_MS
    + ptt2me::constants::CAPTURE_BUFFER_MARGIN_MS
    + ptt2me::constants::RELEASE_GRACE_MS)
    * ptt2me::constants::SAMPLE_RATE as u64
    / 1_000) as usize;
const MAX_WAV_BYTES: u64 = (MAX_AUDIO_FRAMES as u64 * 2) + 4_096;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CorpusEntry {
    id: String,
    wav: PathBuf,
    #[serde(rename = "reference")]
    _reference: serde::de::IgnoredAny,
    duration_seconds: f64,
    source: String,
    sha256: String,
    frames: usize,
    format: String,
}

pub(super) struct CorpusCase {
    pub(super) id: String,
    pub(super) source: String,
    pub(super) duration_seconds: f64,
    pub(super) samples: Vec<f32>,
}

pub(super) fn load(manifest_path: &Path) -> Result<Vec<CorpusCase>, String> {
    let manifest_bytes =
        read_bounded_regular(manifest_path, MAX_MANIFEST_BYTES, "corpus manifest")?;
    let entries: Vec<CorpusEntry> = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| format!("invalid corpus manifest JSON: {error}"))?;
    if entries.is_empty() || entries.len() > MAX_CORPUS_CASES {
        return Err(format!("corpus must contain 1..={MAX_CORPUS_CASES} cases"));
    }

    let base = manifest_path
        .parent()
        .ok_or_else(|| "corpus manifest has no parent directory".to_owned())?;
    let mut ids = std::collections::HashSet::with_capacity(entries.len());
    let mut corpus = Vec::with_capacity(entries.len());
    for entry in entries {
        let CorpusEntry {
            id,
            wav,
            _reference: _,
            duration_seconds,
            source,
            sha256,
            frames,
            format,
        } = entry;
        if id.is_empty()
            || id.len() > MAX_ID_BYTES
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
            || !ids.insert(id.clone())
        {
            return Err("corpus case ids must be unique bounded ASCII identifiers".to_owned());
        }
        if source.is_empty()
            || source.len() > MAX_LABEL_BYTES
            || source.chars().any(char::is_control)
        {
            return Err(
                "corpus source labels must be non-empty bounded single-line text".to_owned(),
            );
        }
        if format != "mono 16000 Hz PCM16 WAV" {
            return Err(format!("corpus case {id} has an unsupported format label"));
        }
        if !is_lowercase_sha256(&sha256) {
            return Err(format!("corpus case {id} has an invalid SHA-256"));
        }
        let wav_path = if wav.is_absolute() {
            wav
        } else {
            base.join(wav)
        };
        let bytes = read_bounded_regular(&wav_path, MAX_WAV_BYTES, "WAV file")?;
        if format!("{:x}", Sha256::digest(&bytes)) != sha256 {
            return Err(format!("corpus case {id} WAV digest mismatch"));
        }
        let decoded = decode_mono_pcm16_wav(&bytes)?;
        if frames == 0 || frames > MAX_AUDIO_FRAMES || frames != decoded.frames {
            return Err(format!("corpus case {id} frame count mismatch or overflow"));
        }
        if !duration_seconds.is_finite()
            || (duration_seconds - frames as f64 / 16_000.0).abs() > 0.5 / 16_000.0
        {
            return Err(format!("corpus case {id} duration mismatch"));
        }
        corpus.push(CorpusCase {
            id,
            source,
            duration_seconds,
            samples: decoded.samples,
        });
    }
    Ok(corpus)
}

fn read_bounded_regular(path: &Path, limit: u64, kind: &str) -> Result<Vec<u8>, String> {
    let path_metadata =
        fs::symlink_metadata(path).map_err(|error| format!("could not inspect {kind}: {error}"))?;
    if !path_metadata.file_type().is_file() || path_metadata.file_type().is_symlink() {
        return Err(format!("{kind} must be a regular non-symlink file"));
    }
    if path_metadata.len() > limit {
        return Err(format!("{kind} exceeds {limit} bytes"));
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| format!("could not open {kind}: {error}"))?;
    let descriptor_metadata = file
        .metadata()
        .map_err(|error| format!("could not inspect open {kind}: {error}"))?;
    if !descriptor_metadata.file_type().is_file() || descriptor_metadata.len() > limit {
        return Err(format!("{kind} changed or exceeds {limit} bytes"));
    }
    let mut bytes = Vec::with_capacity(descriptor_metadata.len() as usize);
    Read::by_ref(&mut file)
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("could not read {kind}: {error}"))?;
    if bytes.len() as u64 > limit || bytes.len() as u64 != descriptor_metadata.len() {
        return Err(format!("{kind} changed or exceeds {limit} bytes"));
    }
    Ok(bytes)
}

fn is_lowercase_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

struct WavData {
    samples: Vec<f32>,
    frames: usize,
}

fn decode_mono_pcm16_wav(bytes: &[u8]) -> Result<WavData, String> {
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("file is not a RIFF/WAVE stream".to_owned());
    }
    let declared_size = read_u32(bytes, 4)? as usize;
    if declared_size.checked_add(8) != Some(bytes.len()) {
        return Err("RIFF size does not match file size".to_owned());
    }

    let mut offset = 12;
    let mut format = None;
    let mut data = None;
    while offset < bytes.len() {
        let header_end = offset
            .checked_add(8)
            .filter(|end| *end <= bytes.len())
            .ok_or_else(|| "truncated WAV chunk header".to_owned())?;
        let chunk_id = &bytes[offset..offset + 4];
        let chunk_len = read_u32(bytes, offset + 4)? as usize;
        let chunk_start = header_end;
        let chunk_end = chunk_start
            .checked_add(chunk_len)
            .filter(|end| *end <= bytes.len())
            .ok_or_else(|| "truncated WAV chunk".to_owned())?;
        match chunk_id {
            b"fmt " if format.is_none() => {
                if chunk_len < 16 {
                    return Err("WAV fmt chunk is too short".to_owned());
                }
                format = Some((
                    read_u16(bytes, chunk_start)?,
                    read_u16(bytes, chunk_start + 2)?,
                    read_u32(bytes, chunk_start + 4)?,
                    read_u32(bytes, chunk_start + 8)?,
                    read_u16(bytes, chunk_start + 12)?,
                    read_u16(bytes, chunk_start + 14)?,
                ));
            }
            b"data" if data.is_none() => data = Some(&bytes[chunk_start..chunk_end]),
            _ => {}
        }
        offset = chunk_end
            .checked_add(chunk_len % 2)
            .filter(|end| *end <= bytes.len())
            .ok_or_else(|| "invalid WAV chunk padding".to_owned())?;
    }

    let (encoding, channels, sample_rate, byte_rate, block_align, bits) =
        format.ok_or_else(|| "WAV fmt chunk is missing".to_owned())?;
    if encoding != 1
        || channels != 1
        || sample_rate != 16_000
        || byte_rate != 32_000
        || block_align != 2
        || bits != 16
    {
        return Err("WAV must be mono 16000 Hz PCM16".to_owned());
    }
    let data = data.ok_or_else(|| "WAV data chunk is missing".to_owned())?;
    if data.len() % 2 != 0 {
        return Err("WAV PCM16 data has an odd byte count".to_owned());
    }
    let samples = data
        .chunks_exact(2)
        .map(|sample| normalize_i16(i16::from_le_bytes([sample[0], sample[1]])))
        .collect::<Vec<_>>();
    Ok(WavData {
        frames: samples.len(),
        samples,
    })
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, String> {
    bytes
        .get(offset..offset + 2)
        .and_then(|value| value.try_into().ok())
        .map(u16::from_le_bytes)
        .ok_or_else(|| "truncated little-endian u16".to_owned())
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, String> {
    bytes
        .get(offset..offset + 4)
        .and_then(|value| value.try_into().ok())
        .map(u32::from_le_bytes)
        .ok_or_else(|| "truncated little-endian u32".to_owned())
}

fn normalize_i16(sample: i16) -> f32 {
    if sample < 0 {
        sample as f32 / -(i16::MIN as f32)
    } else {
        sample as f32 / i16::MAX as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(channels: u16, sample_rate: u32, bits_per_sample: u16, samples: &[i16]) -> Vec<u8> {
        let data_len = (samples.len() * 2) as u32;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&channels.to_le_bytes());
        bytes.extend_from_slice(&sample_rate.to_le_bytes());
        bytes.extend_from_slice(&(sample_rate * u32::from(channels) * 2).to_le_bytes());
        bytes.extend_from_slice(&(channels * 2).to_le_bytes());
        bytes.extend_from_slice(&bits_per_sample.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        for sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        bytes
    }

    fn corpus_fixture() -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let wav_path = directory.path().join("sample.wav");
        let bytes = wav(1, 16_000, 16, &[0; 16_000]);
        fs::write(&wav_path, &bytes).unwrap();
        let digest = format!("{:x}", Sha256::digest(&bytes));
        let manifest_path = directory.path().join("corpus.json");
        let manifest = serde_json::json!([{
            "id": "sample",
            "wav": "sample.wav",
            "reference": "synthetic private reference",
            "duration_seconds": 1.0,
            "source": "synthetic fixture; not human speech corpus",
            "sha256": digest,
            "frames": 16_000,
            "format": "mono 16000 Hz PCM16 WAV"
        }]);
        fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        (directory, manifest_path)
    }

    fn read_manifest(path: &Path) -> serde_json::Value {
        serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
    }

    fn write_manifest(path: &Path, manifest: &serde_json::Value) {
        fs::write(path, serde_json::to_vec(manifest).unwrap()).unwrap();
    }

    fn update_single_case_manifest(
        path: &Path,
        update: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>),
    ) {
        let mut manifest = read_manifest(path);
        let entry = manifest
            .as_array_mut()
            .unwrap()
            .first_mut()
            .unwrap()
            .as_object_mut()
            .unwrap();
        update(entry);
        write_manifest(path, &manifest);
    }

    #[test]
    fn decodes_only_mono_16khz_pcm16_wav() {
        let decoded = decode_mono_pcm16_wav(&wav(1, 16_000, 16, &[-32_768, 0, 32_767])).unwrap();
        assert_eq!(decoded.frames, 3);
        assert_eq!(decoded.samples, vec![-1.0, 0.0, 1.0]);

        assert!(decode_mono_pcm16_wav(&wav(2, 16_000, 16, &[0, 0])).is_err());
        assert!(decode_mono_pcm16_wav(&wav(1, 48_000, 16, &[0])).is_err());
        assert!(decode_mono_pcm16_wav(&wav(1, 16_000, 8, &[0])).is_err());
        assert!(decode_mono_pcm16_wav(b"not a wave").is_err());
    }

    #[test]
    fn constants_keep_corpus_work_bounded() {
        assert_eq!(MAX_CORPUS_CASES, 16);
        assert_eq!(MAX_MANIFEST_BYTES, 65_536);
        assert_eq!(MAX_AUDIO_FRAMES, 418_880);
        assert_eq!(MAX_WAV_BYTES, 841_856);
    }

    #[test]
    fn loads_relative_verified_corpus_without_retaining_reference_text() {
        let (_directory, manifest) = corpus_fixture();
        let corpus = load(&manifest).unwrap();

        assert_eq!(corpus.len(), 1);
        assert_eq!(corpus[0].id, "sample");
        assert_eq!(
            corpus[0].source,
            "synthetic fixture; not human speech corpus"
        );
        assert_eq!(corpus[0].duration_seconds, 1.0);
        assert_eq!(corpus[0].samples.len(), 16_000);
    }

    #[test]
    fn rejects_changed_audio_and_manifest_size_overflow() {
        let (directory, manifest) = corpus_fixture();
        fs::write(
            directory.path().join("sample.wav"),
            wav(1, 16_000, 16, &[1; 16_000]),
        )
        .unwrap();
        assert!(load(&manifest).is_err());

        fs::write(&manifest, vec![b' '; MAX_MANIFEST_BYTES as usize + 1]).unwrap();
        assert!(load(&manifest).is_err());
    }

    #[test]
    fn rejects_manifest_and_wav_symlinks() {
        use std::os::unix::fs::symlink;

        let (directory, manifest) = corpus_fixture();
        let manifest_link = directory.path().join("corpus-link.json");
        symlink(&manifest, &manifest_link).unwrap();
        assert_eq!(
            load(&manifest_link).err().unwrap(),
            "corpus manifest must be a regular non-symlink file"
        );

        let wav_link = directory.path().join("sample-link.wav");
        symlink(directory.path().join("sample.wav"), &wav_link).unwrap();
        update_single_case_manifest(&manifest, |entry| {
            entry.insert("wav".into(), "sample-link.wav".into());
        });
        assert_eq!(
            load(&manifest).err().unwrap(),
            "WAV file must be a regular non-symlink file"
        );
    }

    #[test]
    fn accepts_exact_maximum_wav_bytes_and_frames() {
        const EXPECTED_MAX_AUDIO_FRAMES: usize = 418_880;
        const EXPECTED_MAX_WAV_BYTES: usize = 841_856;

        let (directory, manifest) = corpus_fixture();
        let mut bytes = wav(1, 16_000, 16, &vec![0; EXPECTED_MAX_AUDIO_FRAMES]);
        let extra_bytes = EXPECTED_MAX_WAV_BYTES - bytes.len();
        assert!(extra_bytes >= 8 && (extra_bytes - 8).is_multiple_of(2));
        let mut junk = Vec::with_capacity(extra_bytes);
        junk.extend_from_slice(b"JUNK");
        junk.extend_from_slice(&((extra_bytes - 8) as u32).to_le_bytes());
        junk.resize(extra_bytes, 0);
        bytes.splice(36..36, junk);
        let riff_size = (bytes.len() - 8) as u32;
        bytes[4..8].copy_from_slice(&riff_size.to_le_bytes());
        assert_eq!(bytes.len(), EXPECTED_MAX_WAV_BYTES);

        fs::write(directory.path().join("sample.wav"), &bytes).unwrap();
        let digest = format!("{:x}", Sha256::digest(&bytes));
        update_single_case_manifest(&manifest, |entry| {
            entry.insert(
                "duration_seconds".into(),
                (EXPECTED_MAX_AUDIO_FRAMES as f64 / 16_000.0).into(),
            );
            entry.insert("sha256".into(), digest.into());
            entry.insert("frames".into(), EXPECTED_MAX_AUDIO_FRAMES.into());
        });

        let corpus = load(&manifest).unwrap();
        assert_eq!(corpus[0].samples.len(), EXPECTED_MAX_AUDIO_FRAMES);
    }

    #[test]
    fn rejects_duplicate_and_invalid_case_ids() {
        let (_directory, manifest) = corpus_fixture();
        let original = read_manifest(&manifest);
        let duplicate = serde_json::Value::Array(vec![
            original.as_array().unwrap()[0].clone(),
            original.as_array().unwrap()[0].clone(),
        ]);
        write_manifest(&manifest, &duplicate);
        assert_eq!(
            load(&manifest).err().unwrap(),
            "corpus case ids must be unique bounded ASCII identifiers"
        );

        for invalid_id in [String::new(), "bad/id".into(), "x".repeat(65)] {
            write_manifest(&manifest, &original);
            update_single_case_manifest(&manifest, |entry| {
                entry.insert("id".into(), invalid_id.into());
            });
            assert_eq!(
                load(&manifest).err().unwrap(),
                "corpus case ids must be unique bounded ASCII identifiers"
            );
        }
    }

    #[test]
    fn rejects_invalid_source_labels() {
        let (_directory, manifest) = corpus_fixture();
        let original = read_manifest(&manifest);
        for invalid_source in [String::new(), "two\nlines".into(), "x".repeat(257)] {
            write_manifest(&manifest, &original);
            update_single_case_manifest(&manifest, |entry| {
                entry.insert("source".into(), invalid_source.into());
            });
            assert_eq!(
                load(&manifest).err().unwrap(),
                "corpus source labels must be non-empty bounded single-line text"
            );
        }
    }

    #[test]
    fn rejects_malformed_riff_and_chunk_lengths() {
        let valid = wav(1, 16_000, 16, &[0, 1]);

        let mut wrong_riff_size = valid.clone();
        wrong_riff_size[4..8].copy_from_slice(&0_u32.to_le_bytes());
        assert_eq!(
            decode_mono_pcm16_wav(&wrong_riff_size).err().unwrap(),
            "RIFF size does not match file size"
        );

        let mut truncated_data = valid;
        truncated_data[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(
            decode_mono_pcm16_wav(&truncated_data).err().unwrap(),
            "truncated WAV chunk"
        );
    }
}
