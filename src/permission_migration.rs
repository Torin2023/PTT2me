use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use core_foundation::base::TCFType;
use core_foundation::bundle::{CFBundle, CFBundleGetMainBundle};
use core_foundation::string::CFString;
use objc2::rc::Retained;
use objc2_foundation::{NSString, NSUserDefaults};
use semver::Version;

use crate::constants::BUNDLE_ID;
use crate::state::PermissionSnapshot;

const RESET_MARKER_KEY: &str = "PermissionsResetForBuild";
const SETUP_MARKER_KEY: &str = "PermissionsSetupCompletedForBuild";
const TCCUTIL_PATH: &str = "/usr/bin/tccutil";
const RESET_TIMEOUT: Duration = Duration::from_secs(10);
const RESET_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildIdentity {
    version: String,
    build: String,
    source_commit: String,
}

impl BuildIdentity {
    pub fn parse(
        version: &str,
        build: &str,
        source_commit: &str,
    ) -> Result<Self, BuildIdentityError> {
        let parsed_version =
            Version::parse(version).map_err(|_| BuildIdentityError::InvalidField("version"))?;
        if version.is_empty()
            || !parsed_version.pre.is_empty()
            || !parsed_version.build.is_empty()
            || parsed_version.to_string() != version
        {
            return Err(BuildIdentityError::InvalidField("version"));
        }
        if !is_valid_calendar_minute(build) {
            return Err(BuildIdentityError::InvalidField("build"));
        }
        if !is_lower_hex(source_commit, 40) {
            return Err(BuildIdentityError::InvalidField("source_commit"));
        }

        Ok(Self {
            version: version.to_owned(),
            build: build.to_owned(),
            source_commit: source_commit.to_owned(),
        })
    }

    pub fn marker_value(&self) -> String {
        format!(
            "{}:{}|{}:{}|{}:{}",
            self.version.len(),
            self.version,
            self.build.len(),
            self.build,
            self.source_commit.len(),
            self.source_commit
        )
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn build(&self) -> &str {
        &self.build
    }

    pub fn source_commit(&self) -> &str {
        &self.source_commit
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildIdentityError {
    ExecutableUnavailable,
    BundleUnavailable,
    MissingField(&'static str),
    InvalidField(&'static str),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildIdentityLoad {
    Release(BuildIdentity),
    DevelopmentBypass,
}

pub fn load_main_bundle_identity() -> Result<BuildIdentityLoad, BuildIdentityError> {
    let executable =
        std::env::current_exe().map_err(|_| BuildIdentityError::ExecutableUnavailable)?;
    load_identity_for_executable(&executable, load_identity_from_main_bundle)
}

fn load_identity_for_executable(
    executable: &Path,
    load_bundle_identity: impl FnOnce() -> Result<BuildIdentity, BuildIdentityError>,
) -> Result<BuildIdentityLoad, BuildIdentityError> {
    if !is_app_executable(executable) {
        return Ok(BuildIdentityLoad::DevelopmentBypass);
    }
    load_bundle_identity().map(BuildIdentityLoad::Release)
}

fn is_app_executable(executable: &Path) -> bool {
    let Some(macos) = executable.parent() else {
        return false;
    };
    let Some(contents) = macos.parent() else {
        return false;
    };
    let Some(app) = contents.parent() else {
        return false;
    };
    macos.file_name().is_some_and(|name| name == "MacOS")
        && contents.file_name().is_some_and(|name| name == "Contents")
        && app.extension().is_some_and(|extension| extension == "app")
}

fn load_identity_from_main_bundle() -> Result<BuildIdentity, BuildIdentityError> {
    let bundle_ref = unsafe { CFBundleGetMainBundle() };
    if bundle_ref.is_null() {
        return Err(BuildIdentityError::BundleUnavailable);
    }
    let bundle = unsafe { CFBundle::wrap_under_get_rule(bundle_ref) };
    let info = bundle.info_dictionary();
    let bundle_identifier = required_bundle_string(&info, "CFBundleIdentifier")?;
    if bundle_identifier != BUNDLE_ID {
        return Err(BuildIdentityError::InvalidField("CFBundleIdentifier"));
    }
    let version = required_bundle_string(&info, "CFBundleShortVersionString")?;
    let build = required_bundle_string(&info, "CFBundleVersion")?;
    let source_commit = required_bundle_string(&info, "PTT2meSourceCommit")?;
    BuildIdentity::parse(&version, &build, &source_commit)
}

fn required_bundle_string(
    info: &core_foundation::dictionary::CFDictionary<CFString, core_foundation::base::CFType>,
    field: &'static str,
) -> Result<String, BuildIdentityError> {
    let key = CFString::new(field);
    let value = info
        .find(&key)
        .ok_or(BuildIdentityError::MissingField(field))?;
    value
        .downcast::<CFString>()
        .map(|value| value.to_string())
        .ok_or(BuildIdentityError::InvalidField(field))
}

fn is_lower_hex(value: &str, expected_length: usize) -> bool {
    value.len() == expected_length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_valid_calendar_minute(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 12 || !bytes.iter().all(u8::is_ascii_digit) {
        return false;
    }

    let year = decimal(&bytes[0..4]);
    let month = decimal(&bytes[4..6]);
    let day = decimal(&bytes[6..8]);
    let hour = decimal(&bytes[8..10]);
    let minute = decimal(&bytes[10..12]);
    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => return false,
    };

    year != 0 && (1..=days_in_month).contains(&day) && hour < 24 && minute < 60
}

fn decimal(digits: &[u8]) -> u32 {
    digits
        .iter()
        .fold(0, |value, digit| value * 10 + u32::from(digit - b'0'))
}

const fn is_leap_year(year: u32) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerStoreError {
    WriteFailed,
}

pub trait PermissionMigrationStore {
    fn permissions_reset_for_build(&self) -> Option<String>;
    fn permissions_setup_completed_for_build(&self) -> Option<String>;
    fn set_permissions_reset_for_build(&mut self, value: &str) -> Result<(), MarkerStoreError>;
    fn set_permissions_setup_completed_for_build(
        &mut self,
        value: &str,
    ) -> Result<(), MarkerStoreError>;
}

pub struct SystemPermissionMigrationStore {
    defaults: Retained<NSUserDefaults>,
}

impl SystemPermissionMigrationStore {
    pub fn standard() -> Self {
        Self {
            defaults: unsafe { NSUserDefaults::standardUserDefaults() },
        }
    }

    fn string_for_key(&self, key: &str) -> Option<String> {
        let key = NSString::from_str(key);
        unsafe { self.defaults.stringForKey(&key) }.map(|value| value.to_string())
    }

    fn set_string_for_key(&mut self, key: &str, value: &str) -> Result<(), MarkerStoreError> {
        let key = NSString::from_str(key);
        let value = NSString::from_str(value);
        unsafe {
            self.defaults.setObject_forKey(Some(&value), &key);
            self.defaults
                .synchronize()
                .then_some(())
                .ok_or(MarkerStoreError::WriteFailed)
        }
    }
}

impl Default for SystemPermissionMigrationStore {
    fn default() -> Self {
        Self::standard()
    }
}

impl PermissionMigrationStore for SystemPermissionMigrationStore {
    fn permissions_reset_for_build(&self) -> Option<String> {
        self.string_for_key(RESET_MARKER_KEY)
    }

    fn permissions_setup_completed_for_build(&self) -> Option<String> {
        self.string_for_key(SETUP_MARKER_KEY)
    }

    fn set_permissions_reset_for_build(&mut self, value: &str) -> Result<(), MarkerStoreError> {
        self.set_string_for_key(RESET_MARKER_KEY, value)
    }

    fn set_permissions_setup_completed_for_build(
        &mut self,
        value: &str,
    ) -> Result<(), MarkerStoreError> {
        self.set_string_for_key(SETUP_MARKER_KEY, value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TccService {
    Accessibility,
    ListenEvent,
    Microphone,
}

impl TccService {
    const ORDERED: [Self; 3] = [Self::Accessibility, Self::ListenEvent, Self::Microphone];

    const fn argument(self) -> &'static str {
        match self {
            Self::Accessibility => "Accessibility",
            Self::ListenEvent => "ListenEvent",
            Self::Microphone => "Microphone",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetBoundaryError {
    SpawnFailed,
    CommandFailed,
    WaitFailed,
    TimedOut,
}

pub trait ResetBoundary {
    fn reset(&mut self, service: TccService) -> Result<(), ResetBoundaryError>;
}

pub struct SystemResetBoundary;

impl ResetBoundary for SystemResetBoundary {
    fn reset(&mut self, service: TccService) -> Result<(), ResetBoundaryError> {
        let mut child = Command::new(TCCUTIL_PATH)
            .args(["reset", service.argument(), BUNDLE_ID])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| ResetBoundaryError::SpawnFailed)?;
        wait_for_reset_child(&mut child, RESET_TIMEOUT)
    }
}

fn wait_for_reset_child(child: &mut Child, timeout: Duration) -> Result<(), ResetBoundaryError> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or(ResetBoundaryError::TimedOut)?;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(_)) => return Err(ResetBoundaryError::CommandFailed),
            Ok(None) => {}
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ResetBoundaryError::WaitFailed);
            }
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ResetBoundaryError::TimedOut);
        }
        thread::sleep(RESET_POLL_INTERVAL);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionMigrationSuccess {
    Release(BuildIdentity),
    DevelopmentBypass,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionMigrationError {
    ResetFailed(TccService),
    ResetMarkerWriteFailed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionMigrationRunError {
    Identity(BuildIdentityError),
    Migration(PermissionMigrationError),
}

pub fn run_permission_migration(
    identity: BuildIdentityLoad,
    store: &mut impl PermissionMigrationStore,
    reset: &mut impl ResetBoundary,
) -> Result<PermissionMigrationSuccess, PermissionMigrationError> {
    let BuildIdentityLoad::Release(identity) = identity else {
        return Ok(PermissionMigrationSuccess::DevelopmentBypass);
    };
    let marker = identity.marker_value();
    if store.permissions_reset_for_build().as_deref() == Some(marker.as_str()) {
        return Ok(PermissionMigrationSuccess::Release(identity));
    }

    for service in TccService::ORDERED {
        reset
            .reset(service)
            .map_err(|_| PermissionMigrationError::ResetFailed(service))?;
    }
    store
        .set_permissions_reset_for_build(&marker)
        .map_err(|_| PermissionMigrationError::ResetMarkerWriteFailed)?;
    Ok(PermissionMigrationSuccess::Release(identity))
}

pub fn run_system_permission_migration(
) -> Result<PermissionMigrationSuccess, PermissionMigrationRunError> {
    let identity = load_main_bundle_identity().map_err(PermissionMigrationRunError::Identity)?;
    let mut store = SystemPermissionMigrationStore::standard();
    let mut reset = SystemResetBoundary;
    run_permission_migration(identity, &mut store, &mut reset)
        .map_err(PermissionMigrationRunError::Migration)
}

pub fn persist_setup_completion_if_granted(
    identity: &BuildIdentity,
    permissions: PermissionSnapshot,
    store: &mut impl PermissionMigrationStore,
) -> Result<(), MarkerStoreError> {
    if permissions != PermissionSnapshot::all() {
        return Ok(());
    }
    let marker = identity.marker_value();
    if store.permissions_setup_completed_for_build().as_deref() == Some(marker.as_str()) {
        return Ok(());
    }
    store.set_permissions_setup_completed_for_build(&marker)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::state::PermissionSnapshot;

    use super::*;

    const COMMIT_A: &str = "0123456789abcdef0123456789abcdef01234567";
    const COMMIT_B: &str = "89abcdef0123456789abcdef0123456789abcdef";

    #[derive(Default)]
    struct MemoryStore {
        reset: Option<String>,
        setup: Option<String>,
        reset_writes: usize,
        setup_writes: usize,
        fail_reset_write: bool,
    }

    impl PermissionMigrationStore for MemoryStore {
        fn permissions_reset_for_build(&self) -> Option<String> {
            self.reset.clone()
        }

        fn permissions_setup_completed_for_build(&self) -> Option<String> {
            self.setup.clone()
        }

        fn set_permissions_reset_for_build(&mut self, value: &str) -> Result<(), MarkerStoreError> {
            self.reset_writes += 1;
            if self.fail_reset_write {
                Err(MarkerStoreError::WriteFailed)
            } else {
                self.reset = Some(value.to_owned());
                Ok(())
            }
        }

        fn set_permissions_setup_completed_for_build(
            &mut self,
            value: &str,
        ) -> Result<(), MarkerStoreError> {
            self.setup_writes += 1;
            self.setup = Some(value.to_owned());
            Ok(())
        }
    }

    #[derive(Default)]
    struct RecordingResetBoundary {
        calls: Vec<TccService>,
        fail_on: Option<TccService>,
    }

    impl ResetBoundary for RecordingResetBoundary {
        fn reset(&mut self, service: TccService) -> Result<(), ResetBoundaryError> {
            self.calls.push(service);
            if self.fail_on == Some(service) {
                Err(ResetBoundaryError::CommandFailed)
            } else {
                Ok(())
            }
        }
    }

    fn identity(version: &str, build: &str, commit: &str) -> BuildIdentity {
        BuildIdentity::parse(version, build, commit).unwrap()
    }

    #[test]
    fn first_release_launch_resets_exact_services_then_marks_the_build() {
        let identity = identity("1.0.6", "202608011200", COMMIT_A);
        let mut store = MemoryStore::default();
        let mut reset = RecordingResetBoundary::default();

        assert_eq!(
            run_permission_migration(
                BuildIdentityLoad::Release(identity.clone()),
                &mut store,
                &mut reset,
            ),
            Ok(PermissionMigrationSuccess::Release(identity))
        );
        assert_eq!(
            reset.calls,
            [
                TccService::Accessibility,
                TccService::ListenEvent,
                TccService::Microphone,
            ]
        );
        assert_eq!(
            store.reset.as_deref(),
            Some("5:1.0.6|12:202608011200|40:0123456789abcdef0123456789abcdef01234567")
        );
        assert_eq!(store.reset_writes, 1);
        assert_eq!(store.setup, None);
    }

    #[test]
    fn same_build_relaunch_never_resets_again() {
        let identity = identity("1.0.6", "202608011200", COMMIT_A);
        let mut store = MemoryStore {
            reset: Some(identity.marker_value()),
            ..MemoryStore::default()
        };
        let mut reset = RecordingResetBoundary::default();

        assert!(run_permission_migration(
            BuildIdentityLoad::Release(identity),
            &mut store,
            &mut reset,
        )
        .is_ok());
        assert!(reset.calls.is_empty());
        assert_eq!(store.reset_writes, 0);
    }

    #[test]
    fn changed_build_runs_a_new_reset_sequence() {
        let previous = identity("1.0.6", "202608011200", COMMIT_A);
        let current = identity("1.0.6", "202608011201", COMMIT_B);
        let mut store = MemoryStore {
            reset: Some(previous.marker_value()),
            ..MemoryStore::default()
        };
        let mut reset = RecordingResetBoundary::default();

        run_permission_migration(
            BuildIdentityLoad::Release(current.clone()),
            &mut store,
            &mut reset,
        )
        .unwrap();
        assert_eq!(reset.calls.len(), 3);
        assert_eq!(
            store.reset.as_deref(),
            Some(current.marker_value().as_str())
        );
    }

    #[test]
    fn second_command_failure_stops_and_leaves_no_success_marker() {
        let identity = identity("1.0.6", "202608011200", COMMIT_A);
        let mut store = MemoryStore::default();
        let mut reset = RecordingResetBoundary {
            fail_on: Some(TccService::ListenEvent),
            ..RecordingResetBoundary::default()
        };

        assert_eq!(
            run_permission_migration(BuildIdentityLoad::Release(identity), &mut store, &mut reset,),
            Err(PermissionMigrationError::ResetFailed(
                TccService::ListenEvent
            ))
        );
        assert_eq!(
            reset.calls,
            [TccService::Accessibility, TccService::ListenEvent]
        );
        assert_eq!(store.reset, None);
        assert_eq!(store.reset_writes, 0);
    }

    #[test]
    fn reset_success_with_incomplete_setup_continues_without_another_reset() {
        let identity = identity("1.0.6", "202608011200", COMMIT_A);
        let mut store = MemoryStore {
            reset: Some(identity.marker_value()),
            setup: None,
            ..MemoryStore::default()
        };
        let mut reset = RecordingResetBoundary::default();

        assert_eq!(
            run_permission_migration(
                BuildIdentityLoad::Release(identity.clone()),
                &mut store,
                &mut reset,
            ),
            Ok(PermissionMigrationSuccess::Release(identity))
        );
        assert!(reset.calls.is_empty());
        assert_eq!(store.setup, None);
    }

    #[test]
    fn setup_marker_is_written_only_after_all_three_permissions_are_granted() {
        let identity = identity("1.0.6", "202608011200", COMMIT_A);
        let mut store = MemoryStore {
            reset: Some(identity.marker_value()),
            ..MemoryStore::default()
        };

        persist_setup_completion_if_granted(
            &identity,
            PermissionSnapshot {
                accessibility: true,
                input_monitoring: true,
                microphone: false,
            },
            &mut store,
        )
        .unwrap();
        assert_eq!(store.setup, None);
        assert_eq!(store.setup_writes, 0);

        persist_setup_completion_if_granted(&identity, PermissionSnapshot::all(), &mut store)
            .unwrap();
        persist_setup_completion_if_granted(&identity, PermissionSnapshot::all(), &mut store)
            .unwrap();
        assert_eq!(
            store.setup.as_deref(),
            Some(identity.marker_value().as_str())
        );
        assert_eq!(store.setup_writes, 1);
    }

    #[test]
    fn development_binary_outside_an_app_bypasses_bundle_loading_and_reset() {
        let loaded = load_identity_for_executable(Path::new("/tmp/target/debug/ptt2me"), || {
            panic!("development bypass must not read bundle identity fields")
        })
        .unwrap();
        let mut store = MemoryStore::default();
        let mut reset = RecordingResetBoundary::default();

        assert_eq!(
            run_permission_migration(loaded, &mut store, &mut reset),
            Ok(PermissionMigrationSuccess::DevelopmentBypass)
        );
        assert!(reset.calls.is_empty());
        assert_eq!(store.reset_writes, 0);
    }

    #[test]
    fn reset_marker_write_failure_is_a_migration_failure() {
        let identity = identity("1.0.6", "202608011200", COMMIT_A);
        let mut store = MemoryStore {
            fail_reset_write: true,
            ..MemoryStore::default()
        };
        let mut reset = RecordingResetBoundary::default();

        assert_eq!(
            run_permission_migration(BuildIdentityLoad::Release(identity), &mut store, &mut reset,),
            Err(PermissionMigrationError::ResetMarkerWriteFailed)
        );
        assert_eq!(reset.calls.len(), 3);
        assert_eq!(store.reset, None);
    }

    #[test]
    fn release_identity_rejects_unstable_version_invalid_build_or_commit() {
        for (version, build, commit, expected) in [
            ("1.0.6-beta.1", "202608011200", COMMIT_A, "version"),
            ("1.0.6", "202613011200", COMMIT_A, "build"),
            ("1.0.6", "202608011200", "ABC", "source_commit"),
        ] {
            assert_eq!(
                BuildIdentity::parse(version, build, commit),
                Err(BuildIdentityError::InvalidField(expected))
            );
        }
    }

    #[test]
    fn app_executable_propagates_missing_bundle_identity_fields() {
        assert_eq!(
            load_identity_for_executable(
                Path::new("/Applications/PTT2me.app/Contents/MacOS/PTT2me"),
                || Err(BuildIdentityError::MissingField("CFBundleVersion")),
            ),
            Err(BuildIdentityError::MissingField("CFBundleVersion"))
        );
    }
}
