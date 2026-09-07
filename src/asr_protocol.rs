//! Private, bounded ASR transport. No native error strings cross this boundary.
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Component, Path, PathBuf};

use crate::constants::{CAPTURE_BUFFER_MARGIN_MS, MAX_CAPTURE_MS, RELEASE_GRACE_MS, SAMPLE_RATE};
use crate::model::ModelPaths;

pub const HEADER_LEN: usize = 28;
pub const MAX_PATH_BYTES: usize = 4096;
pub const MAX_TEXT_BYTES: usize = 64 * 1024;
pub const MAX_DIAGNOSTIC_BYTES: usize = 1024;
pub const MAX_SAMPLES: usize = (SAMPLE_RATE as u64
    * (MAX_CAPTURE_MS + RELEASE_GRACE_MS + CAPTURE_BUFFER_MARGIN_MS)
    / 1000) as usize;
const MAGIC: [u8; 4] = *b"PTTA";
const VERSION: u16 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum Kind {
    Hello = 1,
    HelloAck,
    Load,
    Transcribe,
    Loaded,
    Recognized,
    Failure,
    Shutdown,
}

impl Kind {
    fn from_u16(value: u16) -> io::Result<Self> {
        match value {
            1 => Ok(Self::Hello),
            2 => Ok(Self::HelloAck),
            3 => Ok(Self::Load),
            4 => Ok(Self::Transcribe),
            5 => Ok(Self::Loaded),
            6 => Ok(Self::Recognized),
            7 => Ok(Self::Failure),
            8 => Ok(Self::Shutdown),
            _ => Err(invalid()),
        }
    }
    fn bounds(self) -> (usize, usize) {
        match self {
            Self::Hello | Self::HelloAck | Self::Loaded | Self::Shutdown => (0, 0),
            Self::Load => (5, 4 + MAX_PATH_BYTES),
            Self::Transcribe => (4, 4 + MAX_SAMPLES * 4),
            Self::Recognized => (0, MAX_TEXT_BYTES),
            Self::Failure => (2, 2 + MAX_DIAGNOSTIC_BYTES),
        }
    }
}

#[derive(Debug)]
pub struct Header {
    pub kind: Kind,
    pub session: u64,
    pub request: u64,
    pub len: usize,
}
impl Header {
    pub fn decode(bytes: &[u8; HEADER_LEN]) -> io::Result<Self> {
        if bytes[..4] != MAGIC || u16::from_le_bytes(bytes[4..6].try_into().unwrap()) != VERSION {
            return Err(invalid());
        }
        let header = Self {
            kind: Kind::from_u16(u16::from_le_bytes(bytes[6..8].try_into().unwrap()))?,
            session: u64::from_le_bytes(bytes[8..16].try_into().unwrap()),
            request: u64::from_le_bytes(bytes[16..24].try_into().unwrap()),
            len: u32::from_le_bytes(bytes[24..28].try_into().unwrap()) as usize,
        };
        header.validate()?;
        Ok(header)
    }
    fn validate(&self) -> io::Result<()> {
        let (min, max) = self.kind.bounds();
        let handshake = matches!(self.kind, Kind::Hello | Kind::HelloAck);
        if self.session == 0 || (self.request == 0) != handshake || !(min..=max).contains(&self.len)
        {
            return Err(invalid());
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct Frame {
    pub header: Header,
    pub payload: Vec<u8>,
}
impl Frame {
    pub fn new(kind: Kind, session: u64, request: u64, payload: Vec<u8>) -> io::Result<Self> {
        let frame = Self {
            header: Header {
                kind,
                session,
                request,
                len: payload.len(),
            },
            payload,
        };
        frame.header.validate()?;
        frame.validate_payload()?;
        Ok(frame)
    }
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        self.header.validate()?;
        if self.header.len != self.payload.len() {
            return Err(invalid());
        }
        self.validate_payload()?;
        let mut bytes = Vec::with_capacity(HEADER_LEN + self.payload.len());
        bytes.extend_from_slice(&MAGIC);
        bytes.extend_from_slice(&VERSION.to_le_bytes());
        bytes.extend_from_slice(&(self.header.kind as u16).to_le_bytes());
        bytes.extend_from_slice(&self.header.session.to_le_bytes());
        bytes.extend_from_slice(&self.header.request.to_le_bytes());
        bytes.extend_from_slice(&(self.header.len as u32).to_le_bytes());
        bytes.extend_from_slice(&self.payload);
        Ok(bytes)
    }
    pub fn read(reader: &mut impl Read) -> io::Result<Self> {
        let mut bytes = [0; HEADER_LEN];
        reader.read_exact(&mut bytes)?;
        let header = Header::decode(&bytes)?;
        let mut payload = vec![0; header.len];
        reader.read_exact(&mut payload)?;
        Self::new(header.kind, header.session, header.request, payload)
    }
    pub fn write(&self, writer: &mut impl Write) -> io::Result<()> {
        writer.write_all(&self.encode()?)?;
        writer.flush()
    }
    fn validate_payload(&self) -> io::Result<()> {
        match self.header.kind {
            Kind::Load => {
                decode_directory(&self.payload)?;
            }
            Kind::Transcribe => {
                validate_samples(&self.payload)?;
            }
            Kind::Recognized => {
                std::str::from_utf8(&self.payload).map_err(|_| invalid())?;
            }
            Kind::Failure => {
                if u16::from_le_bytes(self.payload[..2].try_into().unwrap()) != 1 {
                    return Err(invalid());
                }
                std::str::from_utf8(&self.payload[2..]).map_err(|_| invalid())?;
            }
            _ => {}
        }
        Ok(())
    }
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "ASR protocol")
}

fn validate_directory(path: &Path) -> io::Result<()> {
    let bytes = path.as_os_str().as_bytes();
    if !path.is_absolute()
        || bytes.is_empty()
        || bytes.len() > MAX_PATH_BYTES
        || bytes.contains(&0)
        || path.components().any(|p| matches!(p, Component::ParentDir))
    {
        return Err(invalid());
    }
    Ok(())
}

pub fn encode_directory(paths: &ModelPaths) -> io::Result<Vec<u8>> {
    let directory = paths.encoder().parent().ok_or_else(invalid)?;
    for (path, name) in [
        (paths.encoder(), "encoder.int8.onnx"),
        (paths.decoder(), "decoder.onnx"),
        (paths.joiner(), "joiner.onnx"),
        (paths.tokens(), "tokens.txt"),
    ] {
        if path.parent() != Some(directory) || path.file_name() != Some(std::ffi::OsStr::new(name))
        {
            return Err(invalid());
        }
    }
    validate_directory(directory)?;
    let bytes = directory.as_os_str().as_bytes();
    let mut payload = (bytes.len() as u32).to_le_bytes().to_vec();
    payload.extend_from_slice(bytes);
    Ok(payload)
}
pub fn decode_directory(bytes: &[u8]) -> io::Result<PathBuf> {
    let length =
        u32::from_le_bytes(bytes.get(..4).ok_or_else(invalid)?.try_into().unwrap()) as usize;
    if length == 0 || length > MAX_PATH_BYTES || length.checked_add(4) != Some(bytes.len()) {
        return Err(invalid());
    }
    let path = PathBuf::from(OsString::from_vec(bytes[4..].to_vec()));
    validate_directory(&path)?;
    Ok(path)
}

pub fn encode_samples(samples: &[f32]) -> io::Result<Vec<u8>> {
    if samples.len() > MAX_SAMPLES || samples.iter().any(|v| !v.is_finite()) {
        return Err(invalid());
    }
    let mut payload = Vec::with_capacity(4 + samples.len() * 4);
    payload.extend_from_slice(&(samples.len() as u32).to_le_bytes());
    for sample in samples {
        payload.extend_from_slice(&sample.to_le_bytes());
    }
    Ok(payload)
}
fn validate_samples(bytes: &[u8]) -> io::Result<()> {
    let count =
        u32::from_le_bytes(bytes.get(..4).ok_or_else(invalid)?.try_into().unwrap()) as usize;
    if count > MAX_SAMPLES
        || count.checked_mul(4).and_then(|v| v.checked_add(4)) != Some(bytes.len())
    {
        return Err(invalid());
    }
    if bytes[4..]
        .chunks_exact(4)
        .any(|v| !f32::from_le_bytes(v.try_into().unwrap()).is_finite())
    {
        return Err(invalid());
    }
    Ok(())
}
pub fn decode_samples(bytes: &[u8]) -> io::Result<Vec<f32>> {
    validate_samples(bytes)?;
    Ok(bytes[4..]
        .chunks_exact(4)
        .map(|v| f32::from_le_bytes(v.try_into().unwrap()))
        .collect())
}

/// Keep a private copy of the protocol pipe, then silence native printf/stdout.
/// Runs before logging, native initialization, TCC or AppKit in the child.
pub fn isolate_stdout() -> io::Result<File> {
    use std::os::fd::AsRawFd;
    let null = File::options().write(true).open("/dev/null")?;
    // SAFETY: fcntl creates a fresh owned descriptor; dup2 replaces only fd 1.
    let fd = unsafe { libc::fcntl(libc::STDOUT_FILENO, libc::F_DUPFD_CLOEXEC, 3) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let output = unsafe { OwnedFd::from_raw_fd(fd) };
    if unsafe { libc::dup2(null.as_raw_fd(), libc::STDOUT_FILENO) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(File::from(output))
}

pub fn run_native_worker() -> i32 {
    use crate::asr::{RecognizerBackend, SherpaRecognizerBackend};
    use crate::model_store::{embedded_model_manifest, verify_model_directory};
    let Ok(mut output) = isolate_stdout() else {
        return 1;
    };
    let mut input = io::stdin().lock();
    let result = (|| -> io::Result<()> {
        let hello = Frame::read(&mut input)?;
        if hello.header.kind != Kind::Hello {
            return Err(invalid());
        }
        let session = hello.header.session;
        Frame::new(Kind::HelloAck, session, 0, vec![])?.write(&mut output)?;
        let mut backend = SherpaRecognizerBackend::default();
        let mut last_request = 0;
        let mut loaded = false;
        loop {
            let frame = Frame::read(&mut input)?;
            let id = frame.header.request;
            if frame.header.session != session || id <= last_request {
                return Err(invalid());
            }
            last_request = id;
            let response = match frame.header.kind {
                Kind::Load if !loaded => {
                    let directory = decode_directory(&frame.payload)?;
                    let paths = embedded_model_manifest()
                        .ok()
                        .and_then(|manifest| verify_model_directory(&directory, &manifest).ok());
                    loaded =
                        paths.is_some_and(|verified| backend.load(verified.into_paths()).is_ok());
                    if loaded {
                        Frame::new(Kind::Loaded, session, id, vec![])?
                    } else {
                        Frame::new(Kind::Failure, session, id, 1_u16.to_le_bytes().to_vec())?
                    }
                }
                Kind::Transcribe if loaded => {
                    let samples = decode_samples(&frame.payload)?;
                    match backend.transcribe(&samples) {
                        Ok(text) if text.trim().len() <= MAX_TEXT_BYTES => Frame::new(
                            Kind::Recognized,
                            session,
                            id,
                            text.trim().as_bytes().to_vec(),
                        )?,
                        _ => Frame::new(Kind::Failure, session, id, 1_u16.to_le_bytes().to_vec())?,
                    }
                }
                Kind::Shutdown => return Ok(()),
                _ => return Err(invalid()),
            };
            response.write(&mut output)?;
        }
    })();
    if result.is_ok() {
        0
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_codec_round_trip_and_malformed_frames() {
        assert_eq!(MAX_SAMPLES, 418_880);
        let samples = [0.0, -0.2, 1.2];
        assert_eq!(
            decode_samples(&encode_samples(&samples).unwrap()).unwrap(),
            samples
        );
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(encode_samples(&[bad]).is_err());
        }
        assert!(encode_samples(&vec![0.0; MAX_SAMPLES + 1]).is_err());
        for count in [1_u32, u32::MAX] {
            assert!(decode_samples(&count.to_le_bytes()).is_err());
        }
        let frame = Frame::new(Kind::Recognized, 2, 3, "привет".as_bytes().to_vec()).unwrap();
        let bytes = frame.encode().unwrap();
        assert_eq!(
            Frame::read(&mut bytes.as_slice()).unwrap().payload,
            frame.payload
        );
        for offset in [0, 4, 6, 8, 16, 24] {
            let mut corrupt = bytes.clone();
            match offset {
                8 => corrupt[8..16].fill(0),
                16 => corrupt[16..24].fill(0),
                _ => corrupt[offset] = 255,
            }
            assert!(
                Frame::read(&mut corrupt.as_slice()).is_err(),
                "offset {offset}"
            );
        }
        for end in 0..bytes.len() {
            assert!(Frame::read(&mut &bytes[..end]).is_err());
        }
        assert!(Frame::new(Kind::Recognized, 1, 1, vec![255]).is_err());
        assert!(Frame::new(Kind::Recognized, 1, 1, vec![0; MAX_TEXT_BYTES + 1]).is_err());
        assert!(Frame::new(Kind::Failure, 1, 1, vec![1; MAX_DIAGNOSTIC_BYTES + 3]).is_err());
    }
    #[test]
    fn decode_rejects_nonfinite_audio_and_bounds_each_frame_before_allocation() {
        for sample in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut bytes = 1_u32.to_le_bytes().to_vec();
            bytes.extend_from_slice(&sample.to_le_bytes());
            assert!(decode_samples(&bytes).is_err());
        }
        for kind in [
            Kind::Load,
            Kind::Transcribe,
            Kind::Recognized,
            Kind::Failure,
        ] {
            let (_, max) = kind.bounds();
            assert!(Header {
                kind,
                session: 1,
                request: 1,
                len: max + 1
            }
            .validate()
            .is_err());
        }
        assert!(Frame::new(Kind::Hello, 1, 1, vec![]).is_err());
        assert!(Frame::new(Kind::HelloAck, 0, 0, vec![]).is_err());
        assert!(Frame::new(Kind::Loaded, 1, 1, vec![0]).is_err());
        let mixed = ModelPaths::for_test(
            "/tmp/a/encoder.int8.onnx".into(),
            "/tmp/b/decoder.onnx".into(),
            "/tmp/a/joiner.onnx".into(),
            "/tmp/a/tokens.txt".into(),
        );
        assert!(encode_directory(&mixed).is_err());
    }

    #[test]
    fn paths_are_bounded_absolute_and_preserve_unix_bytes() {
        for bytes in [b"relative".as_slice(), b"/../bad", b"/bad\0path", b""] {
            let mut payload = (bytes.len() as u32).to_le_bytes().to_vec();
            payload.extend_from_slice(bytes);
            assert!(decode_directory(&payload).is_err());
        }
        let dir = PathBuf::from(OsString::from_vec(b"/tmp/model-\xff".to_vec()));
        let paths = ModelPaths::from_verified_directory(&dir);
        assert_eq!(
            decode_directory(&encode_directory(&paths).unwrap()).unwrap(),
            dir
        );
        let invalid_paths = ModelPaths::for_test(
            dir.join("wrong"),
            dir.join("decoder.onnx"),
            dir.join("joiner.onnx"),
            dir.join("tokens.txt"),
        );
        assert!(encode_directory(&invalid_paths).is_err());
        assert!(
            validate_directory(Path::new(&format!("/{}", "a".repeat(MAX_PATH_BYTES)))).is_err()
        );
    }
}
