use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use serde::Serialize;

const KEY_FILE_LIMIT: u64 = 256;
const PAYLOAD_FILE_LIMIT: u64 = 48 * 1024;
const ENVELOPE_FILE_LIMIT: usize = 64 * 1024;

#[derive(Serialize)]
struct SignedEnvelope {
    schema: u8,
    payload: String,
    signature: String,
}

fn main() -> ExitCode {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    let result = match arguments.as_slice() {
        [mode, private_key, output] if mode == OsStr::new("--derive-public-key") => {
            derive_public_key_file(&PathBuf::from(private_key), &PathBuf::from(output))
        }
        [private_key, payload, output] => sign_payload_file(
            &PathBuf::from(private_key),
            &PathBuf::from(payload),
            &PathBuf::from(output),
        ),
        _ => {
            eprintln!(
                "usage: ptt2me-update-signer PRIVATE_KEY PAYLOAD OUTPUT\n       ptt2me-update-signer --derive-public-key PRIVATE_KEY OUTPUT"
            );
            return ExitCode::from(2);
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("PTT2me update manifest signing failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn sign_payload_file(
    private_key_path: &Path,
    payload_path: &Path,
    output_path: &Path,
) -> Result<(), &'static str> {
    let private_key = read_private_key_seed(private_key_path)?;
    let payload = read_bounded(payload_path, PAYLOAD_FILE_LIMIT, "update payload")?;
    let signing_key = SigningKey::from_bytes(&private_key);
    let signature = signing_key.sign(&payload);
    let mut envelope = serde_json::to_vec(&SignedEnvelope {
        schema: 1,
        payload: STANDARD.encode(&payload),
        signature: STANDARD.encode(signature.to_bytes()),
    })
    .map_err(|_| "could not serialize signed envelope")?;
    envelope.push(b'\n');
    if envelope.len() > ENVELOPE_FILE_LIMIT {
        return Err("signed envelope exceeds 64 KiB");
    }

    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(output_path)
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                "signed manifest output already exists"
            } else {
                "could not create signed manifest output"
            }
        })?;
    if output
        .write_all(&envelope)
        .and_then(|()| output.flush())
        .and_then(|()| output.sync_all())
        .is_err()
    {
        drop(output);
        let _ = fs::remove_file(output_path);
        return Err("could not write signed manifest output");
    }
    Ok(())
}

fn derive_public_key_file(private_key_path: &Path, output_path: &Path) -> Result<(), &'static str> {
    let private_key = read_private_key_seed(private_key_path)?;
    let public_key = SigningKey::from_bytes(&private_key)
        .verifying_key()
        .to_bytes();
    let encoded = format!("{}\n", STANDARD.encode(public_key));
    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(output_path)
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                "public key output already exists"
            } else {
                "could not create public key output"
            }
        })?;
    if output
        .write_all(encoded.as_bytes())
        .and_then(|()| output.flush())
        .and_then(|()| output.sync_all())
        .is_err()
    {
        drop(output);
        let _ = fs::remove_file(output_path);
        return Err("could not write public key output");
    }
    Ok(())
}

fn read_private_key_seed(path: &Path) -> Result<[u8; 32], &'static str> {
    let key_bytes = read_bounded(path, KEY_FILE_LIMIT, "private key")?;
    let key_text = std::str::from_utf8(&key_bytes).map_err(|_| "invalid private key encoding")?;
    let key_line = key_text.strip_suffix('\n').unwrap_or(key_text);
    let key_line = key_line.strip_suffix('\r').unwrap_or(key_line);
    if key_line.is_empty() || key_line.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return Err("private key must contain exactly one base64 line");
    }
    STANDARD
        .decode(key_line)
        .ok()
        .and_then(|decoded| decoded.try_into().ok())
        .ok_or("private key must decode to exactly a 32-byte Ed25519 seed")
}

fn read_bounded(path: &Path, limit: u64, kind: &'static str) -> Result<Vec<u8>, &'static str> {
    let file = File::open(path).map_err(|_| match kind {
        "private key" => "could not read private key",
        _ => "could not read update payload",
    })?;
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "could not read signing input")?;
    if bytes.len() as u64 > limit {
        return Err(match kind {
            "private key" => "private key file is too large",
            _ => "update payload is too large",
        });
    }
    Ok(bytes)
}
