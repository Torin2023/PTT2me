use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use ed25519_dalek::SigningKey;

const FULL_BYTES: &[u8] = b"full dmg integration fixture\n";
const UPDATE_BYTES: &[u8] = b"update dmg integration fixture\n";
const MODEL_MANIFEST_BYTES: &[u8] = b"{\"schema\":1,\"id\":\"gigaam-v3-rnnt-v1\",\"files\":[]}\n";
fn assert_success(output: Output, operation: &str) {
    assert!(
        output.status.success(),
        "{operation} failed ({}):\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run_verifier(
    verifier: &str,
    public_key: &Path,
    manifest: &Path,
    full_dmg: &Path,
    update_dmg: &Path,
    model_manifest: &Path,
) -> Output {
    let library_path = Path::new(verifier)
        .parent()
        .expect("verifier binary directory");
    Command::new(verifier)
        .env("DYLD_LIBRARY_PATH", library_path)
        .arg("--verify-update-manifest")
        .arg(public_key)
        .arg(manifest)
        .arg(full_dmg)
        .arg(update_dmg)
        .arg(model_manifest)
        .output()
        .expect("run hidden update-manifest verifier")
}

fn create_signed_fixture_app(
    root: &Path,
    directory: &str,
    executable_source: &Path,
    resource_marker: &str,
) -> PathBuf {
    let app = root.join(directory).join("Fixture.app");
    let macos = app.join("Contents/MacOS");
    let resources = app.join("Contents/Resources");
    fs::create_dir_all(&macos).unwrap();
    fs::create_dir_all(&resources).unwrap();
    let executable = macos.join("Fixture");
    fs::copy(executable_source, &executable).unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(resources.join("variant.txt"), resource_marker).unwrap();
    fs::write(
        app.join("Contents/Info.plist"),
        concat!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
            "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" ",
            "\"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
            "<plist version=\"1.0\"><dict>",
            "<key>CFBundleExecutable</key><string>Fixture</string>",
            "<key>CFBundleIdentifier</key><string>com.ptt2me.fixture</string>",
            "<key>CFBundlePackageType</key><string>APPL</string>",
            "</dict></plist>\n"
        ),
    )
    .unwrap();
    let signing = Command::new("/usr/bin/codesign")
        .args(["--force", "--sign", "-"])
        .arg(&app)
        .output()
        .expect("ad-hoc sign fixture app");
    assert_success(signing, "fixture app signing");
    app
}

#[test]
fn release_payload_comparison_ignores_only_adhoc_signature_bytes() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    let temp = tempfile::tempdir().unwrap();
    let common_executable = Path::new(env!("CARGO_BIN_EXE_ptt2me-update-signer"));
    let full_app = create_signed_fixture_app(temp.path(), "full", common_executable, "full");
    let update_app = create_signed_fixture_app(temp.path(), "update", common_executable, "update");
    let full_executable = full_app.join("Contents/MacOS/Fixture");
    let update_executable = update_app.join("Contents/MacOS/Fixture");

    assert_ne!(
        fs::read(&full_executable).unwrap(),
        fs::read(&update_executable).unwrap(),
        "different bundle resources must produce different ad-hoc signature bytes"
    );

    let comparison = Command::new(repository.join("scripts/compare-macho-payload.sh"))
        .arg(&full_executable)
        .arg(&update_executable)
        .output()
        .expect("run release payload comparison");
    assert_success(comparison, "same unsigned Mach-O payload comparison");

    let different_app = create_signed_fixture_app(
        temp.path(),
        "different",
        Path::new(env!("CARGO_BIN_EXE_ptt2me")),
        "update",
    );
    let rejected = Command::new(repository.join("scripts/compare-macho-payload.sh"))
        .arg(&full_executable)
        .arg(different_app.join("Contents/MacOS/Fixture"))
        .output()
        .expect("run mismatched release payload comparison");
    assert!(!rejected.status.success());
}

#[test]
fn temporary_key_signing_and_cli_validation_cover_all_release_inputs() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    let temp = tempfile::tempdir().unwrap();
    let private_key = temp.path().join("offline-private-key.txt");
    let public_key = temp.path().join("public-key.txt");
    let payload = temp.path().join("payload.json");
    let manifest = temp.path().join("stable.json");
    let full_dmg = temp.path().join("PTT2me-1.1.0-full-macos-arm64.dmg");
    let update_dmg = temp.path().join("PTT2me-1.1.0-update-macos-arm64.dmg");
    let model_manifest = temp.path().join("gigaam-v3-rnnt-v1.json");

    let signing_key = SigningKey::from_bytes(&[0x42; 32]);
    fs::write(
        &private_key,
        format!("{}\n", STANDARD.encode(signing_key.to_bytes())),
    )
    .unwrap();
    fs::set_permissions(&private_key, fs::Permissions::from_mode(0o600)).unwrap();
    let signer = env!("CARGO_BIN_EXE_ptt2me-update-signer");
    let derivation = Command::new(signer)
        .arg("--derive-public-key")
        .arg(&private_key)
        .arg(&public_key)
        .output()
        .expect("derive public key from temporary private key");
    assert_success(derivation, "public-key derivation");
    fs::write(&full_dmg, FULL_BYTES).unwrap();
    fs::write(&update_dmg, UPDATE_BYTES).unwrap();
    fs::write(&model_manifest, MODEL_MANIFEST_BYTES).unwrap();
    fs::write(
        &payload,
        concat!(
            "{\"channel\":\"stable\",\"version\":\"1.1.0\",",
            "\"build\":202608011200,",
            "\"source_commit\":\"0123456789abcdef0123456789abcdef01234567\",",
            "\"minimum_macos\":\"13.0\",\"architecture\":\"arm64\",",
            "\"required_model\":{\"id\":\"gigaam-v3-rnnt-v1\",",
            "\"manifest_sha256\":\"4fa1a63b755a22d888ffe3e25d9c4cdbc8ea980b4808a9eb82bd2fc4fcce73c8\"},",
            "\"fresh_install\":{",
            "\"url\":\"https://github.com/Torin2023/PTT2me/releases/download/v1.1.0/PTT2me-1.1.0-full-macos-arm64.dmg\",",
            "\"sha256\":\"f7b8b6f9d3736daae40951c3c069b8cc257f55174e991d133a3790ee49428834\",",
            "\"size\":29},",
            "\"application_update\":{",
            "\"url\":\"https://github.com/Torin2023/PTT2me/releases/download/v1.1.0/PTT2me-1.1.0-update-macos-arm64.dmg\",",
            "\"sha256\":\"5c5ac0d18b0c3e7c73b481fba1788c7d8910c23b1a7e95142aafb1f6e051c9aa\",",
            "\"size\":31},",
            "\"published_at\":\"2026-08-01T12:00:00Z\"}\n"
        ),
    )
    .unwrap();

    let verifier = env!("CARGO_BIN_EXE_ptt2me");
    let signing = Command::new(repository.join("scripts/sign-update-manifest.sh"))
        .env("PTT2ME_MANIFEST_SIGNER", signer)
        .arg(&private_key)
        .arg(&payload)
        .arg(&manifest)
        .output()
        .expect("run signing script");
    assert_success(signing, "signing script");

    let verified = run_verifier(
        verifier,
        &public_key,
        &manifest,
        &full_dmg,
        &update_dmg,
        &model_manifest,
    );
    assert_eq!(
        String::from_utf8_lossy(&verified.stdout),
        concat!(
            "version=1.1.0\n",
            "source_commit=0123456789abcdef0123456789abcdef01234567\n"
        )
    );
    assert_success(verified, "hidden verifier");

    let validation = Command::new(repository.join("scripts/validate-update-manifest.sh"))
        .env("PTT2ME_MANIFEST_VERIFIER", verifier)
        .env(
            "PTT2ME_MANIFEST_LIBRARY_PATH",
            Path::new(verifier).parent().unwrap(),
        )
        .arg(&public_key)
        .arg(&manifest)
        .arg(&full_dmg)
        .arg(&update_dmg)
        .arg(&model_manifest)
        .output()
        .expect("run validation script");
    assert_success(validation, "validation script");

    fs::write(
        &model_manifest,
        b"{\"schema\":1,\"id\":\"gigaam-v3-rnnt-v2\",\"files\":[]}\n",
    )
    .unwrap();
    let rejected = run_verifier(
        verifier,
        &public_key,
        &manifest,
        &full_dmg,
        &update_dmg,
        &model_manifest,
    );
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr)
        .contains("required model manifest digest mismatch"));

    let missing_release_inputs =
        Command::new(repository.join("scripts/build-release-artifacts.sh"))
            .output()
            .expect("run release coordinator without inputs");
    assert!(!missing_release_inputs.status.success());
    let release_error = String::from_utf8_lossy(&missing_release_inputs.stderr);
    assert!(release_error.contains("--public-key"));
    assert!(release_error.contains("--private-key"));
}

#[test]
fn release_coordinator_rejects_a_different_public_key_before_building() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    let temp = tempfile::tempdir().unwrap();
    let model_manifest = temp.path().join("model.json");
    let model_source = temp.path().join("model");
    let different_public_key = temp.path().join("different-public-key.txt");
    let private_key = temp.path().join("private-key.txt");
    let output_dir = temp.path().join("output");
    fs::write(&model_manifest, MODEL_MANIFEST_BYTES).unwrap();
    fs::create_dir(&model_source).unwrap();
    fs::create_dir(&output_dir).unwrap();
    let signing_key = SigningKey::from_bytes(&[0x24; 32]);
    fs::write(
        &different_public_key,
        format!(
            "{}\n",
            STANDARD.encode(signing_key.verifying_key().to_bytes())
        ),
    )
    .unwrap();
    fs::write(
        &private_key,
        format!("{}\n", STANDARD.encode(signing_key.to_bytes())),
    )
    .unwrap();
    fs::set_permissions(&private_key, fs::Permissions::from_mode(0o600)).unwrap();

    let output = Command::new(repository.join("scripts/build-release-artifacts.sh"))
        .arg("--version")
        .arg("1.0.5")
        .arg("--build")
        .arg("202608011234")
        .arg("--source-commit")
        .arg("0123456789abcdef0123456789abcdef01234567")
        .arg("--model-manifest")
        .arg(&model_manifest)
        .arg("--model-source")
        .arg(&model_source)
        .arg("--public-key")
        .arg(&different_public_key)
        .arg("--private-key")
        .arg(&private_key)
        .arg("--published-at")
        .arg("2026-08-01T12:34:00Z")
        .arg("--output-dir")
        .arg(&output_dir)
        .output()
        .expect("run release coordinator with a different public key");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("public key must match updates/public-key.txt"),
        "unexpected release coordinator stderr: {stderr}"
    );
    assert!(!stderr.contains("production model manifest is not exact"));
    assert!(fs::read_dir(&output_dir).unwrap().next().is_none());
}
