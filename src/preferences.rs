#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thresholds_are_exact_and_default_to_500_ms() {
        assert_eq!(
            HoldThreshold::OPTIONS.map(HoldThreshold::millis),
            [250, 500, 750]
        );
        assert_eq!(HoldThreshold::default(), HoldThreshold::MS_500);
        assert_eq!(HoldThreshold::from_millis(1_000), None);
    }

    #[test]
    fn trigger_storage_round_trips_and_rejects_excluded_keys() {
        assert_eq!(
            TriggerKey::from_storage_value("fn_globe"),
            Some(TriggerKey::FnGlobe)
        );
        assert_eq!(
            TriggerKey::from_storage_value("keycode:54"),
            Some(TriggerKey::KeyCode(54))
        );
        for code in [53, 57, 72, 73, 74, 127] {
            assert_eq!(TriggerKey::from_keycode(code), None);
        }
        assert_eq!(TriggerKey::from_storage_value("keycode:not-a-number"), None);
    }

    #[test]
    fn invalid_stored_values_fall_back_independently() {
        let loaded = Preferences::from_stored(Some("keycode:57"), Some(1_000));
        assert_eq!(loaded.trigger, TriggerKey::FnGlobe);
        assert_eq!(loaded.threshold, HoldThreshold::MS_500);
    }

    #[derive(Default)]
    struct MemoryRawStore {
        trigger: Option<String>,
        threshold: Option<u64>,
    }

    impl MemoryRawStore {
        fn with(trigger: &str, threshold: u64) -> Self {
            Self {
                trigger: Some(trigger.to_owned()),
                threshold: Some(threshold),
            }
        }
    }

    impl RawPreferenceStore for MemoryRawStore {
        fn trigger_value(&self) -> Option<String> {
            self.trigger.clone()
        }

        fn threshold_value(&self) -> Option<u64> {
            self.threshold
        }

        fn set_trigger_value(&mut self, value: &str) -> Result<(), ()> {
            self.trigger = Some(value.to_owned());
            Ok(())
        }

        fn set_threshold_value(&mut self, value: u64) -> Result<(), ()> {
            self.threshold = Some(value);
            Ok(())
        }
    }

    #[test]
    fn preference_store_loads_and_saves_validated_values() {
        let mut raw = MemoryRawStore::with("keycode:54", 750);
        let mut store = PreferenceRepository::new(raw);
        assert_eq!(
            store.load(),
            Preferences {
                trigger: TriggerKey::KeyCode(54),
                threshold: HoldThreshold::MS_750,
            }
        );
        assert_eq!(store.save(Preferences::default()), Ok(()));
        raw = store.into_inner();
        assert_eq!(raw.trigger.as_deref(), Some("fn_globe"));
        assert_eq!(raw.threshold, Some(500));
    }
}

use core_graphics::event::KeyCode;
use objc2::rc::Retained;
use objc2_foundation::{NSString, NSUserDefaults};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HoldThreshold(u64);

impl HoldThreshold {
    pub const MS_250: Self = Self(250);
    pub const MS_500: Self = Self(500);
    pub const MS_750: Self = Self(750);
    pub const OPTIONS: [Self; 3] = [Self::MS_250, Self::MS_500, Self::MS_750];

    pub const fn millis(self) -> u64 {
        self.0
    }

    pub const fn from_millis(value: u64) -> Option<Self> {
        match value {
            250 => Some(Self::MS_250),
            500 => Some(Self::MS_500),
            750 => Some(Self::MS_750),
            _ => None,
        }
    }
}

impl Default for HoldThreshold {
    fn default() -> Self {
        Self::MS_500
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerKey {
    FnGlobe,
    KeyCode(u16),
}

impl TriggerKey {
    pub const fn from_keycode(keycode: u16) -> Option<Self> {
        match keycode {
            63 | 179 => Some(Self::FnGlobe),
            53 | 57 | 72 | 73 | 74 | 127 => None,
            0..=126 => Some(Self::KeyCode(keycode)),
            _ => None,
        }
    }

    pub fn display_name(self) -> String {
        match self {
            Self::FnGlobe => "Fn / Globe".to_owned(),
            Self::KeyCode(keycode) => fixed_key_name(keycode)
                .map(str::to_owned)
                .unwrap_or_else(|| format!("Клавиша {keycode}")),
        }
    }

    pub fn storage_value(self) -> String {
        match self {
            Self::FnGlobe => "fn_globe".to_owned(),
            Self::KeyCode(keycode) => format!("keycode:{keycode}"),
        }
    }

    pub fn from_storage_value(value: &str) -> Option<Self> {
        if value == "fn_globe" {
            return Some(Self::FnGlobe);
        }

        value
            .strip_prefix("keycode:")?
            .parse::<u16>()
            .ok()
            .and_then(Self::from_keycode)
    }
}

fn fixed_key_name(keycode: u16) -> Option<&'static str> {
    Some(match keycode {
        KeyCode::COMMAND => "Левый Command",
        KeyCode::RIGHT_COMMAND => "Правый Command",
        KeyCode::SHIFT => "Левый Shift",
        KeyCode::RIGHT_SHIFT => "Правый Shift",
        KeyCode::OPTION => "Левый Option",
        KeyCode::RIGHT_OPTION => "Правый Option",
        KeyCode::CONTROL => "Левый Control",
        KeyCode::RIGHT_CONTROL => "Правый Control",
        KeyCode::RETURN => "Return",
        KeyCode::TAB => "Tab",
        KeyCode::SPACE => "Space",
        KeyCode::DELETE => "Delete",
        KeyCode::F1 => "F1",
        KeyCode::F2 => "F2",
        KeyCode::F3 => "F3",
        KeyCode::F4 => "F4",
        KeyCode::F5 => "F5",
        KeyCode::F6 => "F6",
        KeyCode::F7 => "F7",
        KeyCode::F8 => "F8",
        KeyCode::F9 => "F9",
        KeyCode::F10 => "F10",
        KeyCode::F11 => "F11",
        KeyCode::F12 => "F12",
        KeyCode::F13 => "F13",
        KeyCode::F14 => "F14",
        KeyCode::F15 => "F15",
        KeyCode::F16 => "F16",
        KeyCode::F17 => "F17",
        KeyCode::F18 => "F18",
        KeyCode::F19 => "F19",
        KeyCode::F20 => "F20",
        KeyCode::HELP => "Help",
        KeyCode::HOME => "Home",
        KeyCode::PAGE_UP => "Page Up",
        KeyCode::FORWARD_DELETE => "Forward Delete",
        KeyCode::END => "End",
        KeyCode::PAGE_DOWN => "Page Down",
        KeyCode::LEFT_ARROW => "Left Arrow",
        KeyCode::RIGHT_ARROW => "Right Arrow",
        KeyCode::DOWN_ARROW => "Down Arrow",
        KeyCode::UP_ARROW => "Up Arrow",
        _ => return None,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Preferences {
    pub trigger: TriggerKey,
    pub threshold: HoldThreshold,
}

impl Preferences {
    pub fn from_stored(trigger: Option<&str>, threshold: Option<u64>) -> Self {
        Self {
            trigger: trigger
                .and_then(TriggerKey::from_storage_value)
                .unwrap_or_default(),
            threshold: threshold
                .and_then(HoldThreshold::from_millis)
                .unwrap_or_default(),
        }
    }
}

impl Default for TriggerKey {
    fn default() -> Self {
        Self::FnGlobe
    }
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            trigger: TriggerKey::default(),
            threshold: HoldThreshold::default(),
        }
    }
}

pub trait RawPreferenceStore {
    fn trigger_value(&self) -> Option<String>;
    fn threshold_value(&self) -> Option<u64>;
    fn set_trigger_value(&mut self, value: &str) -> Result<(), ()>;
    fn set_threshold_value(&mut self, value: u64) -> Result<(), ()>;
}

pub struct PreferenceRepository<R: RawPreferenceStore> {
    raw: R,
}

impl<R: RawPreferenceStore> PreferenceRepository<R> {
    pub fn new(raw: R) -> Self {
        Self { raw }
    }

    pub fn load(&self) -> Preferences {
        let trigger = self.raw.trigger_value();
        Preferences::from_stored(trigger.as_deref(), self.raw.threshold_value())
    }

    pub fn save(&mut self, preferences: Preferences) -> Result<(), ()> {
        self.raw
            .set_trigger_value(&preferences.trigger.storage_value())?;
        self.raw.set_threshold_value(preferences.threshold.millis())
    }

    #[cfg(test)]
    fn into_inner(self) -> R {
        self.raw
    }
}

pub struct SystemPreferenceStore {
    defaults: Retained<NSUserDefaults>,
}

impl SystemPreferenceStore {
    const TRIGGER_KEY: &'static str = "ptt2me.trigger-key";
    const THRESHOLD_KEY: &'static str = "ptt2me.hold-threshold-ms";

    pub fn new() -> Self {
        Self {
            defaults: unsafe { NSUserDefaults::standardUserDefaults() },
        }
    }
}

impl Default for SystemPreferenceStore {
    fn default() -> Self {
        Self::new()
    }
}

impl RawPreferenceStore for SystemPreferenceStore {
    fn trigger_value(&self) -> Option<String> {
        let key = NSString::from_str(Self::TRIGGER_KEY);
        unsafe { self.defaults.stringForKey(&key) }.map(|value| value.to_string())
    }

    fn threshold_value(&self) -> Option<u64> {
        let key = NSString::from_str(Self::THRESHOLD_KEY);
        unsafe {
            self.defaults.objectForKey(&key)?;
            u64::try_from(self.defaults.integerForKey(&key)).ok()
        }
    }

    fn set_trigger_value(&mut self, value: &str) -> Result<(), ()> {
        let key = NSString::from_str(Self::TRIGGER_KEY);
        let value = NSString::from_str(value);
        unsafe {
            self.defaults.setObject_forKey(Some(&value), &key);
            self.defaults.synchronize().then_some(()).ok_or(())
        }
    }

    fn set_threshold_value(&mut self, value: u64) -> Result<(), ()> {
        let key = NSString::from_str(Self::THRESHOLD_KEY);
        let value = isize::try_from(value).map_err(|_| ())?;
        unsafe {
            self.defaults.setInteger_forKey(value, &key);
            self.defaults.synchronize().then_some(()).ok_or(())
        }
    }
}
