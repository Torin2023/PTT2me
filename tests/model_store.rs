use std::cell::{Cell, RefCell};
use std::fs;
use std::io;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};

use ptt2me::model_store::{
    application_support_root_from_home, embedded_model_manifest, incoming_model_directory,
    model_directory, provision_bundled_model, resolve_model_paths, resolve_model_with_boundary,
    verify_model_directory, ModelManifest, ModelManifestError, ModelStoreBoundary, ModelStoreError,
    ModelVerificationError, SystemModelStoreBoundary, MODEL_ID, MODEL_STORE_SPACE_RESERVE,
};
use ptt2me::update_manifest::ModelAvailability;
use tempfile::TempDir;

const ENCODER: &[u8] = b"enc";
const DECODER: &[u8] = b"dec";
const JOINER: &[u8] = b"join";
const TOKENS: &[u8] = b"tok";

const FIXTURE_MANIFEST: &str = r#"{
  "schema": 1,
  "id": "gigaam-v3-rnnt-v1",
  "files": [
    {
      "name": "encoder.int8.onnx",
      "size": 3,
      "sha256": "5fb2ab76ed9bda034b192c48c7069359252fccda168d925acc0ae7d316c0b53e"
    },
    {
      "name": "decoder.onnx",
      "size": 3,
      "sha256": "e7502c799b8f76fbed077ff2cd55c906ab144d5b88ef09a71abc70b5fad601f1"
    },
    {
      "name": "joiner.onnx",
      "size": 4,
      "sha256": "58393216032be6257784ac0c6a73efb2a084e27b4cfff1e6acee7b7e6ab93b10"
    },
    {
      "name": "tokens.txt",
      "size": 3,
      "sha256": "1a7674eb4ee78df7e1ac439a93c3fa8e3c945784d4dec9fd8e3011738b2f1d62"
    }
  ]
}"#;

fn fixture_manifest() -> ModelManifest {
    ModelManifest::from_bytes(FIXTURE_MANIFEST.as_bytes()).unwrap()
}

fn write_valid_model(directory: &std::path::Path) {
    fs::create_dir_all(directory).unwrap();
    fs::write(directory.join("encoder.int8.onnx"), ENCODER).unwrap();
    fs::write(directory.join("decoder.onnx"), DECODER).unwrap();
    fs::write(directory.join("joiner.onnx"), JOINER).unwrap();
    fs::write(directory.join("tokens.txt"), TOKENS).unwrap();
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailurePoint {
    AvailableSpace,
    Copy,
    QuarantineRename,
    PromoteRename,
    Sync,
    RemoveBackup,
}

struct RecordingBoundary {
    system: SystemModelStoreBoundary,
    available: u64,
    failure: RefCell<Option<FailurePoint>>,
    available_calls: Cell<usize>,
    copy_calls: Cell<usize>,
    rename_calls: RefCell<Vec<(PathBuf, PathBuf)>>,
}

impl RecordingBoundary {
    fn new(available: u64) -> Self {
        Self {
            system: SystemModelStoreBoundary,
            available,
            failure: RefCell::new(None),
            available_calls: Cell::new(0),
            copy_calls: Cell::new(0),
            rename_calls: RefCell::new(Vec::new()),
        }
    }

    fn failing(available: u64, failure: FailurePoint) -> Self {
        let boundary = Self::new(available);
        boundary.failure.replace(Some(failure));
        boundary
    }

    fn should_fail(&self, point: FailurePoint) -> bool {
        self.failure.borrow().as_ref() == Some(&point)
    }
}

impl ModelStoreBoundary for RecordingBoundary {
    fn available_bytes(&self, path: &Path) -> io::Result<u64> {
        self.available_calls.set(self.available_calls.get() + 1);
        let _ = path;
        if self.should_fail(FailurePoint::AvailableSpace) {
            return Err(io::Error::other("injected space query failure"));
        }
        Ok(self.available)
    }

    fn copy_file(&self, source: &Path, destination: &Path, expected_size: u64) -> io::Result<()> {
        self.copy_calls.set(self.copy_calls.get() + 1);
        if self.should_fail(FailurePoint::Copy) && self.copy_calls.get() == 2 {
            return Err(io::Error::other("injected copy failure"));
        }
        self.system.copy_file(source, destination, expected_size)
    }

    fn rename(&self, source: &Path, destination: &Path) -> io::Result<()> {
        self.rename_calls
            .borrow_mut()
            .push((source.to_owned(), destination.to_owned()));
        let source_name = source.file_name().and_then(|name| name.to_str());
        let is_quarantine = source_name == Some(MODEL_ID);
        if (is_quarantine && self.should_fail(FailurePoint::QuarantineRename))
            || (!is_quarantine && self.should_fail(FailurePoint::PromoteRename))
        {
            return Err(io::Error::other("injected rename failure"));
        }
        self.system.rename(source, destination)
    }

    fn sync_directory(&self, path: &Path) -> io::Result<()> {
        if self.should_fail(FailurePoint::Sync) {
            return Err(io::Error::other("injected sync failure"));
        }
        self.system.sync_directory(path)
    }

    fn remove_directory(&self, path: &Path) -> io::Result<()> {
        if self.should_fail(FailurePoint::RemoveBackup)
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains(".invalid-"))
        {
            return Err(io::Error::other("injected backup removal failure"));
        }
        self.system.remove_directory(path)
    }

    fn unique_suffix(&self) -> String {
        "test-unique".to_owned()
    }
}

#[test]
fn embedded_manifest_matches_the_committed_production_contract() {
    let manifest = embedded_model_manifest().unwrap();

    assert_eq!(manifest.schema(), 1);
    assert_eq!(manifest.id(), MODEL_ID);
    assert_eq!(manifest.files().len(), 4);
    assert_eq!(manifest.total_size().unwrap(), 231_897_202);

    let files = manifest.files();
    assert_eq!(files[0].name(), "encoder.int8.onnx");
    assert_eq!(files[0].size(), 224_570_820);
    assert_eq!(
        files[0].sha256(),
        "369f35a71bf288d3b8e0391fabd8dba5f2314088d440bca474056b7b4b6e66bf"
    );
    assert_eq!(files[1].name(), "decoder.onnx");
    assert_eq!(files[1].size(), 4_600_132);
    assert_eq!(
        files[1].sha256(),
        "38fc7475443ea2a26f63211ca350f73ac50fff824ab7a3876ee2bd610c53bbc4"
    );
    assert_eq!(files[2].name(), "joiner.onnx");
    assert_eq!(files[2].size(), 2_712_896);
    assert_eq!(
        files[2].sha256(),
        "602ff7017a93311aad34df1437c8d7f49911353c13d6eae7a6ee7b041339465c"
    );
    assert_eq!(files[3].name(), "tokens.txt");
    assert_eq!(files[3].size(), 13_354);
    assert_eq!(
        files[3].sha256(),
        "39abae20e692998290c574e606f11a9edef2902a1995463fcff63d1490cf22b7"
    );
}

#[test]
fn manifest_parser_rejects_noncanonical_or_ambiguous_input() {
    let cases = [
        (
            "unknown top-level field",
            FIXTURE_MANIFEST.replace("\n  \"files\"", "\n  \"extra\": true,\n  \"files\""),
        ),
        (
            "unknown file field",
            FIXTURE_MANIFEST.replacen("\"size\": 3,", "\"size\": 3,\n      \"extra\": true,", 1),
        ),
        (
            "wrong schema",
            FIXTURE_MANIFEST.replacen("\"schema\": 1", "\"schema\": 2", 1),
        ),
        (
            "wrong id",
            FIXTURE_MANIFEST.replacen(MODEL_ID, "gigaam-v3-rnnt-v2", 1),
        ),
        (
            "duplicate filename",
            FIXTURE_MANIFEST.replacen("decoder.onnx", "encoder.int8.onnx", 1),
        ),
        (
            "unknown filename",
            FIXTURE_MANIFEST.replacen("decoder.onnx", "other.onnx", 1),
        ),
        (
            "zero size",
            FIXTURE_MANIFEST.replacen("\"size\": 3", "\"size\": 0", 1),
        ),
        (
            "uppercase hash",
            FIXTURE_MANIFEST.replacen("5fb2", "5FB2", 1),
        ),
        (
            "short hash",
            FIXTURE_MANIFEST.replacen(
                "5fb2ab76ed9bda034b192c48c7069359252fccda168d925acc0ae7d316c0b53e",
                "5fb2",
                1,
            ),
        ),
    ];

    for (name, json) in cases {
        assert!(
            ModelManifest::from_bytes(json.as_bytes()).is_err(),
            "{name} was accepted"
        );
    }
}

#[test]
fn manifest_requires_exactly_all_four_allowlisted_files() {
    let missing = FIXTURE_MANIFEST.replace(
        r#",
    {
      "name": "tokens.txt",
      "size": 3,
      "sha256": "1a7674eb4ee78df7e1ac439a93c3fa8e3c945784d4dec9fd8e3011738b2f1d62"
    }"#,
        "",
    );
    assert!(matches!(
        ModelManifest::from_bytes(missing.as_bytes()),
        Err(ModelManifestError::InvalidFileSet)
    ));
}

#[test]
fn manifest_total_size_is_checked() {
    let overflowing = FIXTURE_MANIFEST
        .replacen("\"size\": 3", "\"size\": 18446744073709551615", 1)
        .replacen("\"size\": 3", "\"size\": 2", 1);
    let manifest = ModelManifest::from_bytes(overflowing.as_bytes()).unwrap();

    assert_eq!(
        manifest.total_size(),
        Err(ModelManifestError::TotalSizeOverflow)
    );
}

#[test]
fn verification_returns_the_only_constructor_for_model_paths() {
    let temp = TempDir::new().unwrap();
    let model_directory = temp.path().join(MODEL_ID);
    write_valid_model(&model_directory);

    let verified = verify_model_directory(&model_directory, &fixture_manifest()).unwrap();
    let paths = verified.paths();

    assert_eq!(verified.id(), MODEL_ID);
    assert_eq!(verified.directory(), model_directory);
    assert_eq!(
        verified.availability(),
        ModelAvailability::Verified(MODEL_ID.to_owned())
    );
    assert_eq!(paths.encoder(), model_directory.join("encoder.int8.onnx"));
    assert_eq!(paths.decoder(), model_directory.join("decoder.onnx"));
    assert_eq!(paths.joiner(), model_directory.join("joiner.onnx"));
    assert_eq!(paths.tokens(), model_directory.join("tokens.txt"));
}

#[test]
fn verification_rejects_missing_extra_size_hash_and_executable_files() {
    type FixtureMutation = Box<dyn Fn(&Path)>;

    let manifest = fixture_manifest();
    let cases: [(&str, FixtureMutation); 5] = [
        (
            "missing",
            Box::new(|dir| {
                fs::remove_file(dir.join("tokens.txt")).unwrap();
            }),
        ),
        (
            "extra",
            Box::new(|dir| {
                fs::write(dir.join("surprise"), []).unwrap();
            }),
        ),
        (
            "size",
            Box::new(|dir| {
                fs::write(dir.join("tokens.txt"), b"too long").unwrap();
            }),
        ),
        (
            "hash",
            Box::new(|dir| {
                fs::write(dir.join("tokens.txt"), b"bad").unwrap();
            }),
        ),
        (
            "executable",
            Box::new(|dir| {
                let path = dir.join("tokens.txt");
                let mut permissions = fs::metadata(&path).unwrap().permissions();
                permissions.set_mode(0o755);
                fs::set_permissions(path, permissions).unwrap();
            }),
        ),
    ];

    for (name, mutate) in cases {
        let temp = TempDir::new().unwrap();
        let directory = temp.path().join(name);
        write_valid_model(&directory);
        mutate(&directory);
        assert!(
            verify_model_directory(&directory, &manifest).is_err(),
            "{name} was accepted"
        );
    }
}

#[test]
fn verification_never_follows_symlinks() {
    let temp = TempDir::new().unwrap();
    let real_directory = temp.path().join("real");
    write_valid_model(&real_directory);
    let symlinked_directory = temp.path().join("symlinked-directory");
    symlink(&real_directory, &symlinked_directory).unwrap();

    assert!(matches!(
        verify_model_directory(&symlinked_directory, &fixture_manifest()),
        Err(ModelVerificationError::NotDirectory { .. })
    ));

    let linked_file_directory = temp.path().join("linked-file");
    write_valid_model(&linked_file_directory);
    fs::remove_file(linked_file_directory.join("tokens.txt")).unwrap();
    symlink(
        real_directory.join("tokens.txt"),
        linked_file_directory.join("tokens.txt"),
    )
    .unwrap();
    assert!(matches!(
        verify_model_directory(&linked_file_directory, &fixture_manifest()),
        Err(ModelVerificationError::NotRegularFile { .. })
    ));
}

#[test]
fn application_support_path_is_derived_from_an_explicit_home() {
    assert_eq!(
        application_support_root_from_home(Path::new("/Users/example")).unwrap(),
        PathBuf::from("/Users/example/Library/Application Support/PTT2me")
    );
    assert!(application_support_root_from_home(Path::new("relative-home")).is_err());
}

#[test]
fn valid_external_model_wins_without_bundle_or_space_access() {
    let temp = TempDir::new().unwrap();
    let app_support = temp.path().join("app-support");
    let external = model_directory(&app_support);
    write_valid_model(&external);
    let absent_bundle = temp.path().join("bundle-must-not-be-read");
    let boundary = RecordingBoundary::new(0);

    let verified = resolve_model_with_boundary(
        &app_support,
        Some(&absent_bundle),
        &fixture_manifest(),
        &boundary,
    )
    .unwrap();

    assert_eq!(verified.directory(), external);
    assert_eq!(boundary.available_calls.get(), 0);
    assert_eq!(boundary.copy_calls.get(), 0);
}

#[test]
fn named_provision_and_resolve_interfaces_expose_typed_repair() {
    let temp = TempDir::new().unwrap();
    let app_support = temp.path().join("app-support");
    let absent_bundle = temp.path().join("absent-bundle");

    assert!(matches!(
        provision_bundled_model(&app_support, &absent_bundle),
        Err(ModelStoreError::RepairRequired)
    ));
    assert!(matches!(
        resolve_model_paths(&app_support, None),
        Err(ModelStoreError::RepairRequired)
    ));
}

#[test]
fn valid_incoming_is_recovered_without_reserving_space() {
    let temp = TempDir::new().unwrap();
    let app_support = temp.path().join("app-support");
    let incoming = incoming_model_directory(&app_support);
    write_valid_model(&incoming);
    let boundary = RecordingBoundary::new(0);

    let verified =
        resolve_model_with_boundary(&app_support, None, &fixture_manifest(), &boundary).unwrap();

    assert_eq!(verified.directory(), model_directory(&app_support));
    assert!(!incoming.exists());
    assert_eq!(boundary.available_calls.get(), 0);
    assert_eq!(boundary.copy_calls.get(), 0);
}

#[test]
fn invalid_safe_incoming_is_replaced_from_a_verified_bundle() {
    let temp = TempDir::new().unwrap();
    let app_support = temp.path().join("app-support");
    let incoming = incoming_model_directory(&app_support);
    fs::create_dir_all(&incoming).unwrap();
    fs::write(incoming.join("tokens.txt"), b"partial").unwrap();
    let bundle = temp.path().join("bundle");
    write_valid_model(&bundle);
    let manifest = fixture_manifest();
    let boundary =
        RecordingBoundary::new(manifest.total_size().unwrap() + MODEL_STORE_SPACE_RESERVE);

    let verified =
        resolve_model_with_boundary(&app_support, Some(&bundle), &manifest, &boundary).unwrap();

    assert_eq!(verified.directory(), model_directory(&app_support));
    assert_eq!(boundary.copy_calls.get(), 4);
    assert_eq!(
        fs::metadata(verified.directory())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    for file in manifest.files() {
        assert_eq!(
            fs::metadata(verified.directory().join(file.name()))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[test]
fn unsafe_incoming_is_left_for_manual_repair() {
    let temp = TempDir::new().unwrap();
    let app_support = temp.path().join("app-support");
    let incoming = incoming_model_directory(&app_support);
    fs::create_dir_all(&incoming).unwrap();
    fs::write(incoming.join("foreign-file"), b"do not delete").unwrap();
    let bundle = temp.path().join("bundle");
    write_valid_model(&bundle);
    let boundary = RecordingBoundary::new(u64::MAX);

    let result =
        resolve_model_with_boundary(&app_support, Some(&bundle), &fixture_manifest(), &boundary);

    assert!(matches!(
        result,
        Err(ModelStoreError::UnsafeIncoming { .. })
    ));
    assert_eq!(
        fs::read(incoming.join("foreign-file")).unwrap(),
        b"do not delete"
    );
    assert_eq!(boundary.copy_calls.get(), 0);
}

#[test]
fn symlinked_incoming_is_never_followed_or_removed() {
    let temp = TempDir::new().unwrap();
    let app_support = temp.path().join("app-support");
    let real_directory = temp.path().join("outside");
    fs::create_dir_all(app_support.join("models")).unwrap();
    write_valid_model(&real_directory);
    let incoming = incoming_model_directory(&app_support);
    symlink(&real_directory, &incoming).unwrap();

    let result = resolve_model_with_boundary(
        &app_support,
        None,
        &fixture_manifest(),
        &RecordingBoundary::new(0),
    );

    assert!(matches!(
        result,
        Err(ModelStoreError::UnsafeIncoming { .. })
    ));
    assert!(fs::symlink_metadata(&incoming)
        .unwrap()
        .file_type()
        .is_symlink());
    assert!(verify_model_directory(&real_directory, &fixture_manifest()).is_ok());
}

#[test]
fn bundle_is_verified_before_space_or_copy_side_effects() {
    let temp = TempDir::new().unwrap();
    let app_support = temp.path().join("app-support");
    let bundle = temp.path().join("bundle");
    write_valid_model(&bundle);
    fs::write(bundle.join("extra"), b"unexpected").unwrap();
    let boundary = RecordingBoundary::new(u64::MAX);

    let result =
        resolve_model_with_boundary(&app_support, Some(&bundle), &fixture_manifest(), &boundary);

    assert!(matches!(result, Err(ModelStoreError::InvalidBundle(_))));
    assert_eq!(boundary.available_calls.get(), 0);
    assert_eq!(boundary.copy_calls.get(), 0);
    assert!(!model_directory(&app_support).exists());
}

#[test]
fn missing_bundle_is_a_typed_repair_requirement() {
    let temp = TempDir::new().unwrap();
    let app_support = temp.path().join("app-support");
    let final_directory = model_directory(&app_support);
    fs::create_dir_all(&final_directory).unwrap();
    fs::write(final_directory.join("user-note"), b"preserve").unwrap();
    let boundary = RecordingBoundary::new(u64::MAX);

    let result = resolve_model_with_boundary(&app_support, None, &fixture_manifest(), &boundary);

    assert!(matches!(result, Err(ModelStoreError::RepairRequired)));
    assert_eq!(
        fs::read(final_directory.join("user-note")).unwrap(),
        b"preserve"
    );
    assert_eq!(boundary.available_calls.get(), 0);
}

#[test]
fn required_space_is_checked_with_the_fixed_reserve() {
    let temp = TempDir::new().unwrap();
    let app_support = temp.path().join("app-support");
    let bundle = temp.path().join("bundle");
    write_valid_model(&bundle);
    let manifest = fixture_manifest();
    let required = manifest.total_size().unwrap() + MODEL_STORE_SPACE_RESERVE;
    let boundary = RecordingBoundary::new(required - 1);

    let result = resolve_model_with_boundary(&app_support, Some(&bundle), &manifest, &boundary);

    assert!(matches!(
        result,
        Err(ModelStoreError::InsufficientSpace {
            required: actual_required,
            available
        }) if actual_required == required && available == required - 1
    ));
    assert_eq!(boundary.copy_calls.get(), 0);
    assert!(!incoming_model_directory(&app_support).exists());
}

#[test]
fn available_space_query_failure_happens_before_staging_and_preserves_final() {
    let temp = TempDir::new().unwrap();
    let app_support = temp.path().join("app-support");
    let final_directory = model_directory(&app_support);
    fs::create_dir_all(&final_directory).unwrap();
    fs::write(final_directory.join("sentinel"), b"preserve").unwrap();
    let bundle = temp.path().join("bundle");
    write_valid_model(&bundle);
    let boundary = RecordingBoundary::failing(u64::MAX, FailurePoint::AvailableSpace);

    let result =
        resolve_model_with_boundary(&app_support, Some(&bundle), &fixture_manifest(), &boundary);

    assert!(matches!(result, Err(ModelStoreError::Storage { .. })));
    assert_eq!(
        fs::read(final_directory.join("sentinel")).unwrap(),
        b"preserve"
    );
    assert!(!incoming_model_directory(&app_support).exists());
    assert_eq!(boundary.copy_calls.get(), 0);
}

#[test]
fn copy_failure_preserves_an_existing_invalid_final_directory() {
    let temp = TempDir::new().unwrap();
    let app_support = temp.path().join("app-support");
    let final_directory = model_directory(&app_support);
    fs::create_dir_all(&final_directory).unwrap();
    fs::write(final_directory.join("user-note"), b"preserve").unwrap();
    let older_model = app_support.join("models/older-model-id");
    fs::create_dir_all(&older_model).unwrap();
    fs::write(older_model.join("sentinel"), b"older").unwrap();
    let bundle = temp.path().join("bundle");
    write_valid_model(&bundle);
    let boundary = RecordingBoundary::failing(u64::MAX, FailurePoint::Copy);

    let result =
        resolve_model_with_boundary(&app_support, Some(&bundle), &fixture_manifest(), &boundary);

    assert!(matches!(result, Err(ModelStoreError::Storage { .. })));
    assert_eq!(
        fs::read(final_directory.join("user-note")).unwrap(),
        b"preserve"
    );
    assert_eq!(fs::read(older_model.join("sentinel")).unwrap(), b"older");
}

#[test]
fn quarantine_rename_failure_preserves_the_old_final() {
    let temp = TempDir::new().unwrap();
    let app_support = temp.path().join("app-support");
    let final_directory = model_directory(&app_support);
    fs::create_dir_all(&final_directory).unwrap();
    fs::write(final_directory.join("user-note"), b"preserve").unwrap();
    let bundle = temp.path().join("bundle");
    write_valid_model(&bundle);
    let boundary = RecordingBoundary::failing(u64::MAX, FailurePoint::QuarantineRename);

    let result =
        resolve_model_with_boundary(&app_support, Some(&bundle), &fixture_manifest(), &boundary);

    assert!(matches!(result, Err(ModelStoreError::Storage { .. })));
    assert_eq!(
        fs::read(final_directory.join("user-note")).unwrap(),
        b"preserve"
    );
    assert!(
        verify_model_directory(&incoming_model_directory(&app_support), &fixture_manifest())
            .is_ok()
    );
}

#[test]
fn crash_after_quarantine_is_recoverable_without_bundle_or_new_reservation() {
    let temp = TempDir::new().unwrap();
    let app_support = temp.path().join("app-support");
    let final_directory = model_directory(&app_support);
    fs::create_dir_all(&final_directory).unwrap();
    fs::write(final_directory.join("user-note"), b"old invalid").unwrap();
    let bundle = temp.path().join("bundle");
    write_valid_model(&bundle);
    let failing = RecordingBoundary::failing(u64::MAX, FailurePoint::PromoteRename);

    let first =
        resolve_model_with_boundary(&app_support, Some(&bundle), &fixture_manifest(), &failing);
    assert!(matches!(first, Err(ModelStoreError::Storage { .. })));
    assert!(!final_directory.exists());
    assert!(incoming_model_directory(&app_support).exists());
    let backups = fs::read_dir(app_support.join("models"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(&format!("{MODEL_ID}.invalid-")))
        .collect::<Vec<_>>();
    assert_eq!(backups.len(), 1);

    let recovery = RecordingBoundary::new(0);
    let verified =
        resolve_model_with_boundary(&app_support, None, &fixture_manifest(), &recovery).unwrap();

    assert_eq!(verified.directory(), final_directory);
    assert_eq!(recovery.available_calls.get(), 0);
    assert_eq!(recovery.copy_calls.get(), 0);
    assert!(app_support.join("models").join(&backups[0]).exists());
}

#[test]
fn successful_replacement_removes_only_its_recorded_invalid_backup() {
    let temp = TempDir::new().unwrap();
    let app_support = temp.path().join("app-support");
    let models = app_support.join("models");
    let final_directory = model_directory(&app_support);
    fs::create_dir_all(&final_directory).unwrap();
    fs::write(final_directory.join("user-note"), b"invalid").unwrap();
    let unrelated_backup = models.join(format!("{MODEL_ID}.invalid-unrelated"));
    fs::create_dir_all(&unrelated_backup).unwrap();
    fs::write(unrelated_backup.join("sentinel"), b"keep").unwrap();
    let bundle = temp.path().join("bundle");
    write_valid_model(&bundle);
    let boundary = RecordingBoundary::new(u64::MAX);

    let verified =
        resolve_model_with_boundary(&app_support, Some(&bundle), &fixture_manifest(), &boundary)
            .unwrap();

    assert_eq!(verified.directory(), final_directory);
    assert_eq!(
        fs::read(unrelated_backup.join("sentinel")).unwrap(),
        b"keep"
    );
    assert!(!models
        .join(format!("{MODEL_ID}.invalid-test-unique"))
        .exists());
}

#[test]
fn backup_cleanup_failure_keeps_a_verified_final_and_is_reusable() {
    let temp = TempDir::new().unwrap();
    let app_support = temp.path().join("app-support");
    let final_directory = model_directory(&app_support);
    fs::create_dir_all(&final_directory).unwrap();
    fs::write(final_directory.join("sentinel"), b"invalid").unwrap();
    let bundle = temp.path().join("bundle");
    write_valid_model(&bundle);
    let failing = RecordingBoundary::failing(u64::MAX, FailurePoint::RemoveBackup);

    let first =
        resolve_model_with_boundary(&app_support, Some(&bundle), &fixture_manifest(), &failing);
    assert!(matches!(first, Err(ModelStoreError::Storage { .. })));
    assert!(verify_model_directory(&final_directory, &fixture_manifest()).is_ok());
    assert!(app_support
        .join("models")
        .join(format!("{MODEL_ID}.invalid-test-unique"))
        .exists());

    let reuse = RecordingBoundary::new(0);
    let verified =
        resolve_model_with_boundary(&app_support, None, &fixture_manifest(), &reuse).unwrap();
    assert_eq!(verified.directory(), final_directory);
    assert_eq!(reuse.available_calls.get(), 0);
}

#[test]
fn sync_failure_leaves_a_recoverable_staging_state_and_preserves_final() {
    let temp = TempDir::new().unwrap();
    let app_support = temp.path().join("app-support");
    let final_directory = model_directory(&app_support);
    fs::create_dir_all(&final_directory).unwrap();
    fs::write(final_directory.join("user-note"), b"preserve").unwrap();
    let bundle = temp.path().join("bundle");
    write_valid_model(&bundle);
    let failing = RecordingBoundary::failing(u64::MAX, FailurePoint::Sync);

    let first =
        resolve_model_with_boundary(&app_support, Some(&bundle), &fixture_manifest(), &failing);

    assert!(matches!(first, Err(ModelStoreError::Storage { .. })));
    assert_eq!(
        fs::read(final_directory.join("user-note")).unwrap(),
        b"preserve"
    );
    assert!(incoming_model_directory(&app_support).exists());

    let recovery = RecordingBoundary::new(u64::MAX);
    let verified =
        resolve_model_with_boundary(&app_support, Some(&bundle), &fixture_manifest(), &recovery)
            .unwrap();
    assert_eq!(verified.directory(), final_directory);
}
