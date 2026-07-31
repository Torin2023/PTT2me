use objc2::rc::Retained;
use objc2_foundation::{NSString, NSUserDefaults};

const APPEND_SPACE_KEY: &str = "ptt2me.output.append-space";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OutputPreferences {
    pub append_space: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputPreferenceError {
    WriteFailed,
}

pub trait RawOutputPreferenceStore {
    fn append_space(&self) -> Option<bool>;
    fn set_append_space(&mut self, value: bool) -> Result<(), OutputPreferenceError>;
}

pub struct OutputPreferenceRepository<R> {
    raw: R,
}

impl<R: RawOutputPreferenceStore> OutputPreferenceRepository<R> {
    pub fn new(raw: R) -> Self {
        Self { raw }
    }

    pub fn load(&self) -> OutputPreferences {
        OutputPreferences {
            append_space: self.raw.append_space().unwrap_or(false),
        }
    }

    pub fn save(&mut self, value: OutputPreferences) -> Result<(), OutputPreferenceError> {
        self.raw.set_append_space(value.append_space)
    }
}

pub struct OutputPreferenceController<R> {
    current: OutputPreferences,
    repository: OutputPreferenceRepository<R>,
}

impl<R: RawOutputPreferenceStore> OutputPreferenceController<R> {
    pub fn load(repository: OutputPreferenceRepository<R>) -> Self {
        let current = repository.load();
        Self {
            current,
            repository,
        }
    }

    pub const fn current(&self) -> OutputPreferences {
        self.current
    }

    pub fn set_append_space(&mut self, value: bool) -> Result<(), OutputPreferenceError> {
        self.current.append_space = value;
        self.repository.save(self.current)
    }
}

pub struct SystemOutputPreferenceStore {
    defaults: Retained<NSUserDefaults>,
}

impl SystemOutputPreferenceStore {
    pub fn standard() -> Self {
        Self {
            defaults: unsafe { NSUserDefaults::standardUserDefaults() },
        }
    }
}

impl RawOutputPreferenceStore for SystemOutputPreferenceStore {
    fn append_space(&self) -> Option<bool> {
        let key = NSString::from_str(APPEND_SPACE_KEY);
        unsafe {
            self.defaults.objectForKey(&key)?;
            Some(self.defaults.boolForKey(&key))
        }
    }

    fn set_append_space(&mut self, value: bool) -> Result<(), OutputPreferenceError> {
        let key = NSString::from_str(APPEND_SPACE_KEY);
        unsafe {
            self.defaults.setBool_forKey(value, &key);
            self.defaults
                .synchronize()
                .then_some(())
                .ok_or(OutputPreferenceError::WriteFailed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct MemoryRawStore {
        value: Option<bool>,
        fail_writes: bool,
    }

    impl RawOutputPreferenceStore for MemoryRawStore {
        fn append_space(&self) -> Option<bool> {
            self.value
        }

        fn set_append_space(&mut self, value: bool) -> Result<(), OutputPreferenceError> {
            if self.fail_writes {
                Err(OutputPreferenceError::WriteFailed)
            } else {
                self.value = Some(value);
                Ok(())
            }
        }
    }

    #[test]
    fn output_preferences_default_to_no_trailing_space() {
        assert_eq!(
            OutputPreferences::default(),
            OutputPreferences {
                append_space: false,
            }
        );
    }

    #[test]
    fn missing_stored_value_falls_back_to_disabled() {
        let repository = OutputPreferenceRepository::new(MemoryRawStore::default());
        assert_eq!(repository.load(), OutputPreferences::default());
    }

    #[test]
    fn controller_updates_memory_before_persisting() {
        let raw = MemoryRawStore {
            value: Some(false),
            fail_writes: true,
        };
        let mut controller = OutputPreferenceController::load(OutputPreferenceRepository::new(raw));

        assert_eq!(
            controller.set_append_space(true),
            Err(OutputPreferenceError::WriteFailed)
        );
        assert!(controller.current().append_space);
    }

    #[test]
    fn enabled_value_round_trips_through_repository() {
        let mut repository = OutputPreferenceRepository::new(MemoryRawStore::default());
        assert_eq!(
            repository.save(OutputPreferences { append_space: true }),
            Ok(())
        );
        assert!(repository.load().append_space);
    }
}
