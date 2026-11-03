//! A profile choice applies to the next application launch, never a live worker.
use objc2::rc::Retained;
use objc2_foundation::{NSString, NSUserDefaults};

use crate::asr::AsrProfile;

const PROFILE_KEY: &str = "ptt2me.asr.profile";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AsrPreferenceError {
    WriteFailed,
}

pub(crate) trait RawAsrPreferenceStore {
    fn profile(&self) -> Option<String>;
    fn set_profile(&mut self, value: &str) -> Result<(), AsrPreferenceError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct AsrPreferenceSnapshot {
    pub(crate) active: AsrProfile,
    pub(crate) selected: AsrProfile,
    pub(crate) save_failed: bool,
}

pub(crate) struct AsrPreferenceController<R> {
    snapshot: AsrPreferenceSnapshot,
    store: R,
}

impl<R: RawAsrPreferenceStore> AsrPreferenceController<R> {
    pub(crate) fn load(store: R) -> Self {
        let active = store
            .profile()
            .as_deref()
            .and_then(AsrProfile::from_key)
            .unwrap_or_default();
        Self {
            snapshot: AsrPreferenceSnapshot {
                active,
                selected: active,
                save_failed: false,
            },
            store,
        }
    }

    pub(crate) const fn snapshot(&self) -> AsrPreferenceSnapshot {
        self.snapshot
    }

    pub(crate) fn select(&mut self, profile: AsrProfile) -> Result<(), AsrPreferenceError> {
        // This is the requested next-launch choice. NSUserDefaults may retain
        // an in-memory write even when synchronize fails; keep the request
        // visible with an explicit failure, without claiming it was persisted.
        self.snapshot.selected = profile;
        let result = self.store.set_profile(profile.key());
        self.snapshot.save_failed = result.is_err();
        result
    }
}

pub(crate) struct SystemAsrPreferenceStore {
    defaults: Retained<NSUserDefaults>,
}

impl SystemAsrPreferenceStore {
    pub(crate) fn standard() -> Self {
        Self {
            defaults: unsafe { NSUserDefaults::standardUserDefaults() },
        }
    }
}

impl RawAsrPreferenceStore for SystemAsrPreferenceStore {
    fn profile(&self) -> Option<String> {
        let key = NSString::from_str(PROFILE_KEY);
        unsafe { self.defaults.stringForKey(&key) }.map(|value| value.to_string())
    }

    fn set_profile(&mut self, value: &str) -> Result<(), AsrPreferenceError> {
        let key = NSString::from_str(PROFILE_KEY);
        let value = NSString::from_str(value);
        unsafe {
            self.defaults.setObject_forKey(Some(&value), &key);
            self.defaults
                .synchronize()
                .then_some(())
                .ok_or(AsrPreferenceError::WriteFailed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct MemoryStore {
        value: Option<String>,
        fail_writes: bool,
    }

    impl RawAsrPreferenceStore for MemoryStore {
        fn profile(&self) -> Option<String> {
            self.value.clone()
        }

        fn set_profile(&mut self, value: &str) -> Result<(), AsrPreferenceError> {
            if self.fail_writes {
                Err(AsrPreferenceError::WriteFailed)
            } else {
                self.value = Some(value.to_owned());
                Ok(())
            }
        }
    }

    #[test]
    fn missing_or_invalid_preferences_keep_the_two_thread_default() {
        for value in [None, Some(""), Some("4"), Some("FAST"), Some("unknown")] {
            let controller = AsrPreferenceController::load(MemoryStore {
                value: value.map(str::to_owned),
                ..MemoryStore::default()
            });
            assert_eq!(controller.snapshot(), AsrPreferenceSnapshot::default());
            assert_eq!(controller.snapshot().active.cpu_threads(), 2);
        }
    }

    #[test]
    fn selecting_a_profile_does_not_change_this_launch_even_after_multiple_choices() {
        let mut controller = AsrPreferenceController::load(MemoryStore::default());
        for profile in [AsrProfile::Fast, AsrProfile::Balanced, AsrProfile::Fast] {
            controller.select(profile).unwrap();
            assert_eq!(controller.snapshot().active, AsrProfile::Balanced);
            assert_eq!(controller.snapshot().selected, profile);
            assert!(!controller.snapshot().save_failed);
        }
        // A new controller represents the next application launch.
        let relaunched = AsrPreferenceController::load(controller.store);
        assert_eq!(relaunched.snapshot().active, AsrProfile::Fast);
        assert_eq!(relaunched.snapshot().selected, AsrProfile::Fast);
    }

    #[test]
    fn failed_save_is_visible_and_retry_never_changes_the_active_profile() {
        let mut controller = AsrPreferenceController::load(MemoryStore {
            value: Some("fast".to_owned()),
            fail_writes: true,
        });
        assert_eq!(
            controller.select(AsrProfile::Balanced),
            Err(AsrPreferenceError::WriteFailed)
        );
        assert_eq!(controller.snapshot().active, AsrProfile::Fast);
        assert_eq!(controller.snapshot().selected, AsrProfile::Balanced);
        assert!(controller.snapshot().save_failed);
        controller.store.fail_writes = false;
        controller.select(AsrProfile::Balanced).unwrap();
        assert!(!controller.snapshot().save_failed);
        assert_eq!(controller.snapshot().active, AsrProfile::Fast);
        let relaunched = AsrPreferenceController::load(controller.store);
        assert_eq!(relaunched.snapshot().active, AsrProfile::Balanced);
    }
}
