use std::collections::{HashMap, HashSet};
use std::env;
use std::ffi::CString;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::model::ModelPaths;
use crate::update_manifest::ModelAvailability;

pub const MODEL_ID: &str = "gigaam-v3-rnnt-v1";
pub const MODEL_FILENAMES: [&str; 4] = [
    "encoder.int8.onnx",
    "decoder.onnx",
    "joiner.onnx",
    "tokens.txt",
];
pub const EMBEDDED_MODEL_MANIFEST_BYTES: &[u8] =
    include_bytes!("../models/manifests/gigaam-v3-rnnt-v1.json");
pub const PRODUCTION_MODEL_MANIFEST_SHA256: &str =
    "d012004c0706adafdcfa05677f0c10679ef844810e2ebc297f9dc9689150b239";
pub const MODEL_STORE_SPACE_RESERVE: u64 = 64 * 1024 * 1024;

const MODELS_DIRECTORY: &str = "models";
const INCOMING_SUFFIX: &str = ".incoming";
const INVALID_MARKER: &str = ".invalid-";
static UNIQUE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelFile {
    name: String,
    size: u64,
    sha256: String,
}

impl ModelFile {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn sha256(&self) -> &str {
        &self.sha256
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelManifest {
    schema: u64,
    id: String,
    files: Vec<ModelFile>,
}

impl ModelManifest {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ModelManifestError> {
        let raw: RawModelManifest = serde_json::from_slice(bytes)
            .map_err(|error| ModelManifestError::InvalidJson(error.to_string()))?;

        if raw.schema != 1 {
            return Err(ModelManifestError::UnsupportedSchema(raw.schema));
        }
        if raw.id != MODEL_ID {
            return Err(ModelManifestError::UnexpectedModelId(raw.id));
        }

        let mut by_name = HashMap::with_capacity(raw.files.len());
        for file in raw.files {
            if !MODEL_FILENAMES.contains(&file.name.as_str()) {
                return Err(ModelManifestError::InvalidFileSet);
            }
            if file.size == 0 {
                return Err(ModelManifestError::InvalidSize(file.name));
            }
            if !is_lowercase_sha256(&file.sha256) {
                return Err(ModelManifestError::InvalidSha256(file.name));
            }
            let name = file.name.clone();
            if by_name
                .insert(
                    name,
                    ModelFile {
                        name: file.name,
                        size: file.size,
                        sha256: file.sha256,
                    },
                )
                .is_some()
            {
                return Err(ModelManifestError::InvalidFileSet);
            }
        }

        if by_name.len() != MODEL_FILENAMES.len() {
            return Err(ModelManifestError::InvalidFileSet);
        }

        let files = MODEL_FILENAMES
            .iter()
            .map(|name| {
                by_name
                    .remove(*name)
                    .ok_or(ModelManifestError::InvalidFileSet)
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self {
            schema: raw.schema,
            id: raw.id,
            files,
        })
    }

    pub fn schema(&self) -> u64 {
        self.schema
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn files(&self) -> &[ModelFile] {
        &self.files
    }

    pub fn total_size(&self) -> Result<u64, ModelManifestError> {
        self.files.iter().try_fold(0_u64, |total, file| {
            total
                .checked_add(file.size)
                .ok_or(ModelManifestError::TotalSizeOverflow)
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelManifestError {
    InvalidJson(String),
    UnsupportedSchema(u64),
    UnexpectedModelId(String),
    InvalidFileSet,
    InvalidSize(String),
    InvalidSha256(String),
    ProductionDigestMismatch {
        expected: &'static str,
        actual: String,
    },
    TotalSizeOverflow,
}

impl fmt::Display for ModelManifestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidJson(error) => write!(formatter, "invalid model manifest JSON: {error}"),
            Self::UnsupportedSchema(schema) => {
                write!(formatter, "unsupported model manifest schema: {schema}")
            }
            Self::UnexpectedModelId(id) => write!(formatter, "unexpected model id: {id}"),
            Self::InvalidFileSet => formatter.write_str("invalid model manifest file set"),
            Self::InvalidSize(name) => write!(formatter, "invalid model file size: {name}"),
            Self::InvalidSha256(name) => write!(formatter, "invalid model file SHA-256: {name}"),
            Self::ProductionDigestMismatch { expected, actual } => write!(
                formatter,
                "production model manifest digest mismatch: expected {expected}, got {actual}"
            ),
            Self::TotalSizeOverflow => formatter.write_str("model manifest total size overflow"),
        }
    }
}

impl std::error::Error for ModelManifestError {}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawModelManifest {
    schema: u64,
    id: String,
    files: Vec<RawModelFile>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawModelFile {
    name: String,
    size: u64,
    sha256: String,
}

pub fn embedded_model_manifest() -> Result<ModelManifest, ModelManifestError> {
    validate_production_model_manifest(EMBEDDED_MODEL_MANIFEST_BYTES)
}

pub fn validate_production_model_manifest(
    bytes: &[u8],
) -> Result<ModelManifest, ModelManifestError> {
    let actual = format!("{:x}", Sha256::digest(bytes));
    if actual != PRODUCTION_MODEL_MANIFEST_SHA256 {
        return Err(ModelManifestError::ProductionDigestMismatch {
            expected: PRODUCTION_MODEL_MANIFEST_SHA256,
            actual,
        });
    }
    ModelManifest::from_bytes(bytes)
}

#[derive(Debug, Clone)]
pub struct VerifiedModel {
    id: String,
    directory: PathBuf,
    paths: ModelPaths,
}

impl VerifiedModel {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn paths(&self) -> &ModelPaths {
        &self.paths
    }

    pub fn into_paths(self) -> ModelPaths {
        self.paths
    }

    pub fn availability(&self) -> ModelAvailability {
        ModelAvailability::Verified(self.id.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelVerificationError {
    Metadata {
        path: PathBuf,
        message: String,
    },
    NotDirectory {
        path: PathBuf,
    },
    ReadDirectory {
        path: PathBuf,
        message: String,
    },
    UnexpectedEntry {
        name: String,
    },
    MissingFile {
        name: String,
    },
    NotRegularFile {
        path: PathBuf,
    },
    ExecutableFile {
        path: PathBuf,
    },
    OpenFile {
        path: PathBuf,
        message: String,
    },
    ReadFile {
        path: PathBuf,
        message: String,
    },
    SizeMismatch {
        path: PathBuf,
        expected: u64,
        actual: u64,
    },
    HashMismatch {
        path: PathBuf,
    },
    FileChanged {
        path: PathBuf,
    },
}

impl fmt::Display for ModelVerificationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Metadata { path, message } => {
                write!(formatter, "could not inspect {}: {message}", path.display())
            }
            Self::NotDirectory { path } => {
                write!(
                    formatter,
                    "model path is not a real directory: {}",
                    path.display()
                )
            }
            Self::ReadDirectory { path, message } => {
                write!(formatter, "could not read {}: {message}", path.display())
            }
            Self::UnexpectedEntry { name } => write!(formatter, "unexpected model entry: {name}"),
            Self::MissingFile { name } => write!(formatter, "missing model file: {name}"),
            Self::NotRegularFile { path } => {
                write!(
                    formatter,
                    "model entry is not a regular file: {}",
                    path.display()
                )
            }
            Self::ExecutableFile { path } => {
                write!(formatter, "model file is executable: {}", path.display())
            }
            Self::OpenFile { path, message } => {
                write!(formatter, "could not open {}: {message}", path.display())
            }
            Self::ReadFile { path, message } => {
                write!(formatter, "could not read {}: {message}", path.display())
            }
            Self::SizeMismatch {
                path,
                expected,
                actual,
            } => write!(
                formatter,
                "model file size mismatch for {}: expected {expected}, got {actual}",
                path.display()
            ),
            Self::HashMismatch { path } => {
                write!(formatter, "model file digest mismatch: {}", path.display())
            }
            Self::FileChanged { path } => {
                write!(
                    formatter,
                    "model file changed during verification: {}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for ModelVerificationError {}

pub fn verify_model_directory(
    directory: &Path,
    manifest: &ModelManifest,
) -> Result<VerifiedModel, ModelVerificationError> {
    let metadata =
        fs::symlink_metadata(directory).map_err(|error| ModelVerificationError::Metadata {
            path: directory.to_owned(),
            message: error.to_string(),
        })?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(ModelVerificationError::NotDirectory {
            path: directory.to_owned(),
        });
    }

    let mut found = HashSet::with_capacity(MODEL_FILENAMES.len());
    let entries =
        fs::read_dir(directory).map_err(|error| ModelVerificationError::ReadDirectory {
            path: directory.to_owned(),
            message: error.to_string(),
        })?;
    for entry in entries {
        let entry = entry.map_err(|error| ModelVerificationError::ReadDirectory {
            path: directory.to_owned(),
            message: error.to_string(),
        })?;
        let name = entry.file_name().into_string().map_err(|name| {
            ModelVerificationError::UnexpectedEntry {
                name: name.to_string_lossy().into_owned(),
            }
        })?;
        if !MODEL_FILENAMES.contains(&name.as_str()) {
            return Err(ModelVerificationError::UnexpectedEntry { name });
        }
        found.insert(name);
    }

    for expected in MODEL_FILENAMES {
        if !found.contains(expected) {
            return Err(ModelVerificationError::MissingFile {
                name: expected.to_owned(),
            });
        }
    }

    for file in manifest.files() {
        verify_file(directory, file)?;
    }

    Ok(VerifiedModel {
        id: manifest.id().to_owned(),
        directory: directory.to_owned(),
        paths: ModelPaths::from_verified_directory(directory),
    })
}

fn verify_file(directory: &Path, expected: &ModelFile) -> Result<(), ModelVerificationError> {
    let path = directory.join(expected.name());
    let path_metadata =
        fs::symlink_metadata(&path).map_err(|error| ModelVerificationError::Metadata {
            path: path.clone(),
            message: error.to_string(),
        })?;
    if !path_metadata.file_type().is_file() || path_metadata.file_type().is_symlink() {
        return Err(ModelVerificationError::NotRegularFile { path });
    }
    if path_metadata.permissions().mode() & 0o111 != 0 {
        return Err(ModelVerificationError::ExecutableFile { path });
    }

    let mut handle =
        open_read_only_nofollow(&path).map_err(|error| ModelVerificationError::OpenFile {
            path: path.clone(),
            message: error.to_string(),
        })?;
    let descriptor_metadata =
        handle
            .metadata()
            .map_err(|error| ModelVerificationError::Metadata {
                path: path.clone(),
                message: error.to_string(),
            })?;
    if !descriptor_metadata.file_type().is_file()
        || descriptor_metadata.permissions().mode() & 0o111 != 0
    {
        return Err(ModelVerificationError::NotRegularFile { path });
    }
    if descriptor_metadata.len() != expected.size() {
        return Err(ModelVerificationError::SizeMismatch {
            path,
            expected: expected.size(),
            actual: descriptor_metadata.len(),
        });
    }
    let initial_snapshot = FileSnapshot::from(&descriptor_metadata);

    let digest = digest_reader_bounded(&mut handle, expected.size()).map_err(|error| {
        ModelVerificationError::ReadFile {
            path: path.clone(),
            message: error.to_string(),
        }
    })?;
    let final_metadata = handle
        .metadata()
        .map_err(|error| ModelVerificationError::Metadata {
            path: path.clone(),
            message: error.to_string(),
        })?;
    if FileSnapshot::from(&final_metadata) != initial_snapshot {
        return Err(ModelVerificationError::FileChanged { path });
    }
    if digest.exceeded_expected_size || digest.bytes_read != expected.size() {
        return Err(ModelVerificationError::SizeMismatch {
            path,
            expected: expected.size(),
            actual: digest.bytes_read,
        });
    }
    if digest.sha256 != expected.sha256() {
        return Err(ModelVerificationError::HashMismatch { path });
    }

    Ok(())
}

pub(crate) fn open_read_only_nofollow(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    options.open(path)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileSnapshot {
    device: u64,
    inode: u64,
    mode: u32,
    size: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl From<&fs::Metadata> for FileSnapshot {
    fn from(metadata: &fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            size: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BoundedDigest {
    sha256: String,
    bytes_read: u64,
    exceeded_expected_size: bool,
}

fn digest_reader_bounded(reader: &mut impl Read, expected_size: u64) -> io::Result<BoundedDigest> {
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 128 * 1024];
    let mut bytes_read = 0_u64;
    let mut limited = reader.take(expected_size.saturating_add(1));
    loop {
        let read = limited.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        bytes_read = bytes_read
            .checked_add(read as u64)
            .ok_or_else(|| io::Error::other("model read length overflow"))?;
        hasher.update(&buffer[..read]);
    }
    Ok(BoundedDigest {
        sha256: format!("{:x}", hasher.finalize()),
        bytes_read,
        exceeded_expected_size: bytes_read > expected_size,
    })
}

fn is_lowercase_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelStoreError {
    InvalidHome(PathBuf),
    Environment(String),
    Manifest(ModelManifestError),
    InvalidBundle(ModelVerificationError),
    InvalidStaging(ModelVerificationError),
    InvalidFinal(ModelVerificationError),
    RepairRequired,
    InsufficientSpace {
        required: u64,
        available: u64,
    },
    UnsafeIncoming {
        path: PathBuf,
    },
    UncontrolledPath {
        path: PathBuf,
        reason: String,
    },
    BackupCollision {
        path: PathBuf,
    },
    Storage {
        operation: &'static str,
        path: PathBuf,
        message: String,
    },
}

impl fmt::Display for ModelStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidHome(path) => {
                write!(formatter, "HOME must be an absolute path: {}", path.display())
            }
            Self::Environment(message) => {
                write!(formatter, "model environment is unavailable: {message}")
            }
            Self::Manifest(error) => write!(formatter, "invalid embedded model manifest: {error}"),
            Self::InvalidBundle(error) => write!(formatter, "invalid bundled model: {error}"),
            Self::InvalidStaging(error) => write!(formatter, "invalid staged model: {error}"),
            Self::InvalidFinal(error) => write!(formatter, "invalid promoted model: {error}"),
            Self::RepairRequired => formatter.write_str("matching Full package is required"),
            Self::InsufficientSpace { required, available } => write!(
                formatter,
                "insufficient space for model: required {required} bytes, available {available} bytes"
            ),
            Self::UnsafeIncoming { path } => write!(
                formatter,
                "unsafe incoming model directory requires manual repair: {}",
                path.display()
            ),
            Self::UncontrolledPath { path, reason } => write!(
                formatter,
                "uncontrolled external model path {}: {reason}",
                path.display()
            ),
            Self::BackupCollision { path } => {
                write!(formatter, "model backup path already exists: {}", path.display())
            }
            Self::Storage {
                operation,
                path,
                message,
            } => write!(
                formatter,
                "model storage operation {operation} failed for {}: {message}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for ModelStoreError {}

impl From<ModelManifestError> for ModelStoreError {
    fn from(error: ModelManifestError) -> Self {
        Self::Manifest(error)
    }
}

pub trait ModelStoreBoundary {
    fn available_bytes(&self, path: &Path) -> io::Result<u64>;
    fn copy_file(&self, source: &Path, destination: &Path, expected_size: u64) -> io::Result<()>;
    fn rename(&self, source: &Path, destination: &Path) -> io::Result<()>;
    fn sync_directory(&self, path: &Path) -> io::Result<()>;
    fn remove_directory(&self, path: &Path) -> io::Result<()>;
    fn unique_suffix(&self) -> String;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemModelStoreBoundary;

impl ModelStoreBoundary for SystemModelStoreBoundary {
    fn available_bytes(&self, path: &Path) -> io::Result<u64> {
        let path = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
        let mut statistics = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        // SAFETY: `path` is a valid NUL-terminated string and `statistics` points to
        // writable storage for one `statvfs` value.
        let result = unsafe { libc::statvfs(path.as_ptr(), statistics.as_mut_ptr()) };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a successful `statvfs` call initialized the value.
        let statistics = unsafe { statistics.assume_init() };
        u64::from(statistics.f_bavail)
            .checked_mul(statistics.f_frsize)
            .ok_or_else(|| io::Error::other("available-space calculation overflow"))
    }

    fn copy_file(&self, source: &Path, destination: &Path, expected_size: u64) -> io::Result<()> {
        let mut source_file = open_read_only_nofollow(source)?;
        let source_metadata = source_file.metadata()?;
        if !source_metadata.file_type().is_file()
            || source_metadata.permissions().mode() & 0o111 != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "source is not a non-executable regular file",
            ));
        }
        if source_metadata.len() != expected_size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "source size does not match the manifest",
            ));
        }
        let source_snapshot = FileSnapshot::from(&source_metadata);

        let mut options = OpenOptions::new();
        options
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        let mut destination_file = options.open(destination)?;
        destination_file.set_permissions(fs::Permissions::from_mode(0o600))?;
        let copied = io::copy(
            &mut (&mut source_file).take(expected_size.saturating_add(1)),
            &mut destination_file,
        )?;
        if copied != expected_size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "source size changed while copying",
            ));
        }
        if FileSnapshot::from(&source_file.metadata()?) != source_snapshot {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "source changed while copying",
            ));
        }
        destination_file.sync_all()
    }

    fn rename(&self, source: &Path, destination: &Path) -> io::Result<()> {
        fs::rename(source, destination)
    }

    fn sync_directory(&self, path: &Path) -> io::Result<()> {
        let mut options = OpenOptions::new();
        options
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC);
        options.open(path)?.sync_all()
    }

    fn remove_directory(&self, path: &Path) -> io::Result<()> {
        fs::remove_dir_all(path)
    }

    fn unique_suffix(&self) -> String {
        let sequence = UNIQUE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        format!("{}-{nanos}-{sequence}", std::process::id())
    }
}

pub fn application_support_root_from_home(home: &Path) -> Result<PathBuf, ModelStoreError> {
    if !home.is_absolute() {
        return Err(ModelStoreError::InvalidHome(home.to_owned()));
    }
    Ok(home.join("Library/Application Support/PTT2me"))
}

pub fn application_support_root() -> Result<PathBuf, ModelStoreError> {
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| ModelStoreError::InvalidHome(PathBuf::new()))?;
    application_support_root_from_home(&home)
}

pub fn model_directory(application_support_root: &Path) -> PathBuf {
    model_store_root(application_support_root).join(MODEL_ID)
}

pub fn incoming_model_directory(application_support_root: &Path) -> PathBuf {
    model_store_root(application_support_root).join(format!("{MODEL_ID}{INCOMING_SUFFIX}"))
}

pub fn bundled_model_directory(resources_directory: &Path) -> PathBuf {
    resources_directory.join(MODELS_DIRECTORY).join(MODEL_ID)
}

pub fn resolve_model(
    application_support_root: &Path,
    bundled_directory: Option<&Path>,
) -> Result<VerifiedModel, ModelStoreError> {
    let manifest = embedded_model_manifest()?;
    resolve_model_with_boundary(
        application_support_root,
        bundled_directory,
        &manifest,
        &SystemModelStoreBoundary,
    )
}

pub fn provision_bundled_model(
    application_support_root: &Path,
    bundled_directory: &Path,
) -> Result<VerifiedModel, ModelStoreError> {
    resolve_model(application_support_root, Some(bundled_directory))
}

pub fn resolve_model_paths(
    application_support_root: &Path,
    bundled_directory: Option<&Path>,
) -> Result<ModelPaths, ModelStoreError> {
    resolve_model(application_support_root, bundled_directory).map(VerifiedModel::into_paths)
}

pub fn resolve_model_with_boundary<B: ModelStoreBoundary>(
    application_support_root: &Path,
    bundled_directory: Option<&Path>,
    manifest: &ModelManifest,
    boundary: &B,
) -> Result<VerifiedModel, ModelStoreError> {
    validate_existing_controlled_directory(application_support_root)?;
    let models_root = model_store_root(application_support_root);
    validate_existing_controlled_directory(&models_root)?;

    let final_directory = model_directory(application_support_root);
    validate_existing_external_model(&final_directory)?;
    if let Ok(verified) = verify_model_directory(&final_directory, manifest) {
        return Ok(verified);
    }

    let incoming_directory = incoming_model_directory(application_support_root);
    if path_exists_nofollow(&incoming_directory)? {
        let incoming_metadata = fs::symlink_metadata(&incoming_directory)
            .map_err(|error| storage("inspect incoming model", &incoming_directory, error))?;
        if incoming_metadata.file_type().is_dir() && !incoming_metadata.file_type().is_symlink() {
            validate_existing_external_model(&incoming_directory)?;
        }
        match verify_model_directory(&incoming_directory, manifest) {
            Ok(_) => {
                ensure_models_root(&models_root)?;
                return promote_incoming(
                    &models_root,
                    &final_directory,
                    &incoming_directory,
                    manifest,
                    boundary,
                );
            }
            Err(_) if incoming_is_safe_to_remove(&incoming_directory) => {
                revalidate_transaction_directory(&models_root, &incoming_directory)?;
                if !incoming_is_safe_to_remove(&incoming_directory) {
                    return Err(ModelStoreError::UnsafeIncoming {
                        path: incoming_directory,
                    });
                }
                boundary
                    .remove_directory(&incoming_directory)
                    .map_err(|error| {
                        storage("remove invalid incoming", &incoming_directory, error)
                    })?;
                boundary
                    .sync_directory(&models_root)
                    .map_err(|error| storage("sync model store", &models_root, error))?;
            }
            Err(_) => {
                return Err(ModelStoreError::UnsafeIncoming {
                    path: incoming_directory,
                });
            }
        }
    }

    let bundled_directory = match bundled_directory {
        Some(path) if path_exists_nofollow(path)? => path,
        _ => return Err(ModelStoreError::RepairRequired),
    };
    verify_model_directory(bundled_directory, manifest).map_err(ModelStoreError::InvalidBundle)?;

    ensure_models_root(&models_root)?;
    revalidate_controlled_store(&models_root)?;
    let required = manifest
        .total_size()?
        .checked_add(MODEL_STORE_SPACE_RESERVE)
        .ok_or(ModelManifestError::TotalSizeOverflow)?;
    let available = boundary
        .available_bytes(&models_root)
        .map_err(|error| storage("query available space", &models_root, error))?;
    if available < required {
        return Err(ModelStoreError::InsufficientSpace {
            required,
            available,
        });
    }

    create_staging_directory(&incoming_directory)?;
    boundary
        .sync_directory(&models_root)
        .map_err(|error| storage("sync model store", &models_root, error))?;
    for file in manifest.files() {
        let source = bundled_directory.join(file.name());
        let destination = incoming_directory.join(file.name());
        boundary
            .copy_file(&source, &destination, file.size())
            .map_err(|error| storage("copy model file", &destination, error))?;
    }
    boundary
        .sync_directory(&incoming_directory)
        .map_err(|error| storage("sync staging directory", &incoming_directory, error))?;
    verify_model_directory(&incoming_directory, manifest)
        .map_err(ModelStoreError::InvalidStaging)?;

    promote_incoming(
        &models_root,
        &final_directory,
        &incoming_directory,
        manifest,
        boundary,
    )
}

fn model_store_root(application_support_root: &Path) -> PathBuf {
    application_support_root.join(MODELS_DIRECTORY)
}

fn ensure_models_root(models_root: &Path) -> Result<(), ModelStoreError> {
    let application_support_root = models_root.parent().ok_or_else(|| {
        storage(
            "locate application support root",
            models_root,
            io::Error::new(io::ErrorKind::InvalidInput, "model store has no parent"),
        )
    })?;
    ensure_controlled_directory(application_support_root, "create application support root")?;
    ensure_controlled_directory(models_root, "create model store")
}

fn ensure_controlled_directory(
    path: &Path,
    operation: &'static str,
) -> Result<(), ModelStoreError> {
    if validate_existing_controlled_directory(path)? {
        return Ok(());
    }
    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700);
    builder
        .create(path)
        .map_err(|error| storage(operation, path, error))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| storage("set controlled directory mode", path, error))?;
    validate_existing_controlled_directory(path)?;
    Ok(())
}

fn create_staging_directory(path: &Path) -> Result<(), ModelStoreError> {
    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700);
    builder
        .create(path)
        .map_err(|error| storage("create staging directory", path, error))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| storage("set staging directory mode", path, error))?;
    validate_existing_controlled_directory(path)?;
    Ok(())
}

fn promote_incoming<B: ModelStoreBoundary>(
    models_root: &Path,
    final_directory: &Path,
    incoming_directory: &Path,
    manifest: &ModelManifest,
    boundary: &B,
) -> Result<VerifiedModel, ModelStoreError> {
    let backup = if path_exists_nofollow(final_directory)? {
        revalidate_transaction_directory(models_root, final_directory)?;
        revalidate_transaction_directory(models_root, incoming_directory)?;
        let backup = models_root.join(format!(
            "{MODEL_ID}{INVALID_MARKER}{}",
            boundary.unique_suffix()
        ));
        if path_exists_nofollow(&backup)? {
            return Err(ModelStoreError::BackupCollision { path: backup });
        }
        revalidate_transaction_directory(models_root, final_directory)?;
        boundary
            .rename(final_directory, &backup)
            .map_err(|error| storage("quarantine invalid model", final_directory, error))?;
        boundary
            .sync_directory(models_root)
            .map_err(|error| storage("sync model store", models_root, error))?;
        Some(backup)
    } else {
        None
    };

    revalidate_transaction_directory(models_root, incoming_directory)?;
    boundary
        .rename(incoming_directory, final_directory)
        .map_err(|error| storage("promote staged model", incoming_directory, error))?;
    boundary
        .sync_directory(models_root)
        .map_err(|error| storage("sync model store", models_root, error))?;
    revalidate_transaction_directory(models_root, final_directory)?;
    let verified =
        verify_model_directory(final_directory, manifest).map_err(ModelStoreError::InvalidFinal)?;

    if let Some(backup) = backup {
        revalidate_transaction_directory(models_root, &backup)?;
        boundary
            .remove_directory(&backup)
            .map_err(|error| storage("remove invalid backup", &backup, error))?;
        boundary
            .sync_directory(models_root)
            .map_err(|error| storage("sync model store", models_root, error))?;
    }

    Ok(verified)
}

fn validate_existing_controlled_directory(path: &Path) -> Result<bool, ModelStoreError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(storage("inspect controlled directory", path, error)),
    };
    if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
        return Err(uncontrolled(path, "expected a real directory"));
    }
    validate_controlled_metadata(path, &metadata)?;
    Ok(true)
}

fn validate_existing_external_model(path: &Path) -> Result<(), ModelStoreError> {
    if !validate_existing_controlled_directory(path)? {
        return Ok(());
    }
    for name in MODEL_FILENAMES {
        let file = path.join(name);
        let metadata = match fs::symlink_metadata(&file) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(storage("inspect controlled model file", &file, error)),
        };
        if metadata.file_type().is_file() && !metadata.file_type().is_symlink() {
            validate_controlled_metadata(&file, &metadata)?;
        }
    }
    Ok(())
}

fn validate_controlled_metadata(
    path: &Path,
    metadata: &fs::Metadata,
) -> Result<(), ModelStoreError> {
    // SAFETY: `geteuid` has no preconditions and does not mutate process state.
    let current_uid = unsafe { libc::geteuid() };
    if metadata.uid() != current_uid {
        return Err(uncontrolled(
            path,
            format!(
                "owner uid {} does not match current uid {current_uid}",
                metadata.uid()
            ),
        ));
    }
    if metadata.permissions().mode() & 0o022 != 0 {
        return Err(uncontrolled(path, "group or world writable"));
    }
    Ok(())
}

fn revalidate_controlled_store(models_root: &Path) -> Result<(), ModelStoreError> {
    let application_support_root = models_root.parent().ok_or_else(|| {
        storage(
            "locate application support root",
            models_root,
            io::Error::new(io::ErrorKind::InvalidInput, "model store has no parent"),
        )
    })?;
    if !validate_existing_controlled_directory(application_support_root)? {
        return Err(uncontrolled(
            application_support_root,
            "application support root disappeared",
        ));
    }
    if !validate_existing_controlled_directory(models_root)? {
        return Err(uncontrolled(models_root, "model store disappeared"));
    }
    Ok(())
}

fn revalidate_transaction_directory(
    models_root: &Path,
    directory: &Path,
) -> Result<(), ModelStoreError> {
    revalidate_controlled_store(models_root)?;
    if !validate_existing_controlled_directory(directory)? {
        return Err(uncontrolled(directory, "transaction directory disappeared"));
    }
    validate_existing_external_model(directory)
}

fn uncontrolled(path: &Path, reason: impl Into<String>) -> ModelStoreError {
    ModelStoreError::UncontrolledPath {
        path: path.to_owned(),
        reason: reason.into(),
    }
}

fn incoming_is_safe_to_remove(path: &Path) -> bool {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => return false,
    };
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return false;
    }

    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(_) => return false,
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => return false,
        };
        let name = match entry.file_name().into_string() {
            Ok(name) => name,
            Err(_) => return false,
        };
        if !MODEL_FILENAMES.contains(&name.as_str()) {
            return false;
        }
        let metadata = match fs::symlink_metadata(entry.path()) {
            Ok(metadata) => metadata,
            Err(_) => return false,
        };
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            return false;
        }
    }
    true
}

fn path_exists_nofollow(path: &Path) -> Result<bool, ModelStoreError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(storage("inspect path", path, error)),
    }
}

fn storage(operation: &'static str, path: &Path, error: io::Error) -> ModelStoreError {
    ModelStoreError::Storage {
        operation,
        path: path.to_owned(),
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{self, Read};

    use tempfile::tempdir;

    use super::{digest_reader_bounded, ModelStoreBoundary, SystemModelStoreBoundary};

    struct EndlessReader {
        read_calls: usize,
    }

    impl Read for EndlessReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            self.read_calls += 1;
            buffer.fill(b'x');
            Ok(buffer.len())
        }
    }

    #[test]
    fn hashing_reads_at_most_expected_size_plus_one() {
        let mut reader = EndlessReader { read_calls: 0 };

        let outcome = digest_reader_bounded(&mut reader, 3).unwrap();

        assert_eq!(outcome.bytes_read, 4);
        assert!(outcome.exceeded_expected_size);
        assert_eq!(reader.read_calls, 1);
    }

    #[test]
    fn system_copy_rechecks_size_and_never_copies_past_expected_plus_one() {
        let temp = tempdir().unwrap();
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        fs::write(&source, b"unexpected growth").unwrap();

        let error = SystemModelStoreBoundary
            .copy_file(&source, &destination, 3)
            .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(fs::metadata(destination).map_or(true, |metadata| metadata.len() <= 4));
    }
}
