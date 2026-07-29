use std::ptr::NonNull;
use std::sync::mpsc::Sender;

use objc2::{
    declare_class, msg_send, msg_send_id, mutability, rc::Retained, runtime::AnyClass,
    runtime::AnyObject, sel, ClassType, DeclaredClass,
};
use objc2_app_kit::{
    NSApplication, NSColor, NSControlStateValueOff, NSControlStateValueOn, NSImage,
    NSImageSymbolConfiguration, NSMenu, NSMenuItem, NSStatusBar, NSStatusBarButton, NSStatusItem,
    NSVariableStatusItemLength,
};
use objc2_foundation::{MainThreadMarker, NSObject, NSObjectProtocol, NSString};

use crate::hotkey::HotkeyControl;
use crate::preferences::{HoldThreshold, Preferences, TriggerKey};
use crate::state::{AppStatus, PermissionKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolStyle {
    Template,
    HierarchicalRed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuProjection {
    pub title: String,
    pub symbol: &'static str,
    pub pulse: bool,
    pub style: SymbolStyle,
}

impl MenuProjection {
    pub fn from_status(status: &AppStatus) -> Self {
        let (title, symbol, pulse, style) = match status {
            AppStatus::Starting => (
                "● Запуск…".into(),
                "hourglass",
                false,
                SymbolStyle::Template,
            ),
            AppStatus::PermissionBlocked(permission) => (
                format!("● Нужен доступ: {}", permission_title(*permission)),
                "exclamationmark.triangle.fill",
                false,
                SymbolStyle::HierarchicalRed,
            ),
            AppStatus::Ready => ("● Готово".into(), "mic", false, SymbolStyle::Template),
            AppStatus::Recording => (
                "● Запись…".into(),
                "record.circle.fill",
                false,
                SymbolStyle::HierarchicalRed,
            ),
            AppStatus::Recognizing => (
                "● Распознавание…".into(),
                "waveform",
                true,
                SymbolStyle::Template,
            ),
            AppStatus::Error {
                message,
                recoverable: false,
            } => (
                format!("● Ошибка: {message}"),
                "exclamationmark.triangle.fill",
                false,
                SymbolStyle::HierarchicalRed,
            ),
            AppStatus::Error {
                message,
                recoverable: true,
            } => (
                format!("● Ошибка: {message}"),
                "exclamationmark.circle.fill",
                false,
                SymbolStyle::Template,
            ),
        };

        Self {
            title,
            symbol,
            pulse,
            style,
        }
    }
}

const fn permission_title(permission: PermissionKind) -> &'static str {
    match permission {
        PermissionKind::Accessibility => "универсальный доступ",
        PermissionKind::InputMonitoring => "мониторинг ввода",
        PermissionKind::Microphone => "микрофон",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuEntry {
    Status,
    Version,
    Trigger,
    Threshold,
    Separator,
    Quit,
}

pub const MENU_DESCRIPTOR: [MenuEntry; 6] = [
    MenuEntry::Status,
    MenuEntry::Version,
    MenuEntry::Trigger,
    MenuEntry::Threshold,
    MenuEntry::Separator,
    MenuEntry::Quit,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuCommand {
    BeginTriggerAssignment,
    ResetTrigger,
    SetThreshold(HoldThreshold),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PreferenceProjection {
    trigger_title: String,
    threshold_states: [bool; 3],
}

impl From<Preferences> for PreferenceProjection {
    fn from(preferences: Preferences) -> Self {
        Self {
            trigger_title: preferences.trigger.display_name(),
            threshold_states: HoldThreshold::OPTIONS.map(|value| value == preferences.threshold),
        }
    }
}

fn symbol_style(projection: &MenuProjection) -> SymbolStyle {
    projection.style
}

struct MenuTargetIvars {
    publisher: MenuCommandPublisher,
}

#[derive(Clone)]
struct MenuCommandPublisher {
    sender: Sender<MenuCommand>,
    hotkey: HotkeyControl,
}

impl MenuCommandPublisher {
    fn new(sender: Sender<MenuCommand>, hotkey: HotkeyControl) -> Self {
        Self { sender, hotkey }
    }

    fn send(&self, command: MenuCommand) -> bool {
        let accepted = match command {
            MenuCommand::BeginTriggerAssignment => self.hotkey.begin_assignment().is_some(),
            MenuCommand::ResetTrigger => {
                self.hotkey.set_trigger(TriggerKey::FnGlobe);
                true
            }
            MenuCommand::SetThreshold(threshold) => {
                self.hotkey.set_threshold(threshold);
                true
            }
        };
        accepted && self.sender.send(command).is_ok()
    }
}

declare_class!(
    struct MenuTarget;

    // SAFETY: NSObject has no subclassing requirements, and this target is
    // created, retained, invoked, and destroyed exclusively on AppKit's main
    // thread.
    unsafe impl ClassType for MenuTarget {
        type Super = NSObject;
        type Mutability = mutability::MainThreadOnly;
        const NAME: &'static str = "PTT2meMenuTarget";
    }

    impl DeclaredClass for MenuTarget {
        type Ivars = MenuTargetIvars;
    }

    unsafe impl NSObjectProtocol for MenuTarget {}

    // SAFETY: `quit:` has the Cocoa action signature `(id) -> void` and is
    // invoked by AppKit on the main thread.
    unsafe impl MenuTarget {
        #[method(quit:)]
        fn quit(&self, sender: &AnyObject) {
            let app = NSApplication::sharedApplication(MainThreadMarker::from(self));
            unsafe { app.terminate(Some(sender)) };
        }

        #[method(assignTrigger:)]
        fn assign_trigger(&self, _sender: &AnyObject) {
            self.ivars()
                .publisher
                .send(MenuCommand::BeginTriggerAssignment);
        }

        #[method(resetTrigger:)]
        fn reset_trigger(&self, _sender: &AnyObject) {
            self.ivars().publisher.send(MenuCommand::ResetTrigger);
        }

        #[method(selectThreshold:)]
        fn select_threshold(&self, sender: &AnyObject) {
            let tag: isize = unsafe { msg_send![sender, tag] };
            let Ok(milliseconds) = u64::try_from(tag) else {
                return;
            };
            let Some(threshold) = HoldThreshold::from_millis(milliseconds) else {
                return;
            };
            self.ivars()
                .publisher
                .send(MenuCommand::SetThreshold(threshold));
        }
    }
);

impl MenuTarget {
    fn new(
        mtm: MainThreadMarker,
        sender: Sender<MenuCommand>,
        hotkey: HotkeyControl,
    ) -> Retained<Self> {
        let this = mtm.alloc().set_ivars(MenuTargetIvars {
            publisher: MenuCommandPublisher::new(sender, hotkey),
        });
        unsafe { msg_send_id![super(this), init] }
    }
}

/// Owns the permanent status menu and projects application and preference state
/// onto its retained rows without rebuilding it.
pub struct MenuBar {
    _status_bar: Retained<NSStatusBar>,
    _status_item: Retained<NSStatusItem>,
    _menu: Retained<NSMenu>,
    status_row: Retained<NSMenuItem>,
    current_trigger_row: Retained<NSMenuItem>,
    threshold_rows: [Retained<NSMenuItem>; 3],
    button: Retained<NSStatusBarButton>,
    _target: Retained<MenuTarget>,
    pulse_active: bool,
}

impl MenuBar {
    /// Creates the status item and its immutable menu.
    ///
    /// # Panics
    ///
    /// Panics when called outside the AppKit main thread or if AppKit does not
    /// provide a button for its newly-created status item.
    pub fn new(
        preferences: Preferences,
        sender: Sender<MenuCommand>,
        hotkey: HotkeyControl,
    ) -> Self {
        let mtm = main_thread_marker();
        let target = MenuTarget::new(mtm, sender, hotkey);
        let menu = unsafe { NSMenu::initWithTitle(mtm.alloc(), &NSString::from_str("PTT2me")) };
        unsafe { menu.setAutoenablesItems(false) };

        let mut status_row = None;
        let mut current_trigger_row = None;
        let mut threshold_rows = Vec::with_capacity(HoldThreshold::OPTIONS.len());
        for entry in MENU_DESCRIPTOR {
            match entry {
                MenuEntry::Status => {
                    let item = menu_item(mtm, "● Запуск…", None);
                    unsafe { item.setEnabled(false) };
                    menu.addItem(&item);
                    status_row = Some(item);
                }
                MenuEntry::Version => {
                    let item = menu_item(mtm, concat!("PTT2me ", env!("CARGO_PKG_VERSION")), None);
                    unsafe { item.setEnabled(false) };
                    menu.addItem(&item);
                }
                MenuEntry::Trigger => {
                    let parent = menu_item(mtm, "Клавиша активации", None);
                    let submenu = unsafe {
                        NSMenu::initWithTitle(mtm.alloc(), &NSString::from_str("Клавиша активации"))
                    };
                    unsafe { submenu.setAutoenablesItems(false) };

                    let current = menu_item(mtm, "", None);
                    unsafe { current.setEnabled(false) };
                    submenu.addItem(&current);

                    let assign = menu_item(mtm, "Назначить…", Some(sel!(assignTrigger:)));
                    unsafe {
                        assign.setTarget(Some(&target));
                        assign.setEnabled(true);
                    }
                    submenu.addItem(&assign);

                    let reset = menu_item(mtm, "Сбросить на Fn / Globe", Some(sel!(resetTrigger:)));
                    unsafe {
                        reset.setTarget(Some(&target));
                        reset.setEnabled(true);
                    }
                    submenu.addItem(&reset);

                    parent.setSubmenu(Some(&submenu));
                    menu.addItem(&parent);
                    current_trigger_row = Some(current);
                }
                MenuEntry::Threshold => {
                    let parent = menu_item(mtm, "Порог удержания", None);
                    let submenu = unsafe {
                        NSMenu::initWithTitle(mtm.alloc(), &NSString::from_str("Порог удержания"))
                    };
                    unsafe { submenu.setAutoenablesItems(false) };

                    for threshold in HoldThreshold::OPTIONS {
                        let item = menu_item(
                            mtm,
                            &format!("{} мс", threshold.millis()),
                            Some(sel!(selectThreshold:)),
                        );
                        unsafe {
                            item.setTarget(Some(&target));
                            item.setTag(threshold.millis() as isize);
                            item.setEnabled(true);
                        }
                        submenu.addItem(&item);
                        threshold_rows.push(item);
                    }

                    parent.setSubmenu(Some(&submenu));
                    menu.addItem(&parent);
                }
                MenuEntry::Separator => menu.addItem(&NSMenuItem::separatorItem(mtm)),
                MenuEntry::Quit => {
                    let item = menu_item(mtm, "Выйти", Some(sel!(quit:)));
                    unsafe {
                        item.setTarget(Some(&target));
                        item.setEnabled(true);
                    }
                    menu.addItem(&item);
                }
            }
        }

        let status_bar = unsafe { NSStatusBar::systemStatusBar() };
        let status_item = unsafe { status_bar.statusItemWithLength(NSVariableStatusItemLength) };
        let button = unsafe {
            status_item
                .button(mtm)
                .expect("AppKit must provide a status item button")
        };
        unsafe {
            button.setTitle(&NSString::from_str(""));
            status_item.setMenu(Some(&menu));
        }

        let mut menu_bar = Self {
            _status_bar: status_bar,
            _status_item: status_item,
            _menu: menu,
            status_row: status_row.expect("menu descriptor must contain the status row"),
            current_trigger_row: current_trigger_row
                .expect("menu descriptor must contain the current trigger row"),
            threshold_rows: threshold_rows
                .try_into()
                .unwrap_or_else(|_| panic!("menu descriptor must contain three threshold rows")),
            button,
            _target: target,
            pulse_active: false,
        };
        menu_bar.render(&AppStatus::Starting);
        menu_bar.render_preferences(preferences);
        menu_bar
    }

    /// Updates only the already-created status row and status-item button.
    ///
    /// The menu descriptor is consumed only by [`Self::new`], so state changes
    /// cannot add, remove, or rebuild menu entries.
    pub fn render(&mut self, status: &AppStatus) {
        let _mtm = main_thread_marker();

        let projection = MenuProjection::from_status(status);
        unsafe {
            self.status_row
                .setTitle(&NSString::from_str(&projection.title));
        }

        if let Some(image) = system_symbol(&projection) {
            unsafe { self.button.setImage(Some(&image)) };
        }

        if self.pulse_active != projection.pulse {
            set_recognition_pulse(&self.button, projection.pulse);
            self.pulse_active = projection.pulse;
        }
    }

    pub fn render_assignment(&mut self) {
        let _mtm = main_thread_marker();
        unsafe {
            self.status_row
                .setTitle(&NSString::from_str("● Нажмите клавишу…"));
        }
    }

    pub fn render_preferences(&mut self, preferences: Preferences) {
        let _mtm = main_thread_marker();
        let projection = PreferenceProjection::from(preferences);
        unsafe {
            self.current_trigger_row
                .setTitle(&NSString::from_str(&format!(
                    "Текущая: {}",
                    projection.trigger_title
                )));
        }
        for (row, selected) in self.threshold_rows.iter().zip(projection.threshold_states) {
            unsafe {
                row.setState(if selected {
                    NSControlStateValueOn
                } else {
                    NSControlStateValueOff
                });
            }
        }
    }
}

fn main_thread_marker() -> MainThreadMarker {
    assert_eq!(
        unsafe { libc::pthread_main_np() },
        1,
        "MenuBar must be used on the main thread"
    );
    // SAFETY: `pthread_main_np` just proved that this is the process main
    // thread. objc2-foundation's safe constructor requires its optional
    // `NSThread` feature, which this minimal application does not otherwise
    // need.
    unsafe { MainThreadMarker::new_unchecked() }
}

fn menu_item(
    mtm: MainThreadMarker,
    title: &str,
    action: Option<objc2::runtime::Sel>,
) -> Retained<NSMenuItem> {
    unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            mtm.alloc(),
            &NSString::from_str(title),
            action,
            &NSString::from_str(""),
        )
    }
}

fn system_symbol(projection: &MenuProjection) -> Option<Retained<NSImage>> {
    let name = NSString::from_str(projection.symbol);
    let description = NSString::from_str(&projection.title);
    let image = unsafe {
        NSImage::imageWithSystemSymbolName_accessibilityDescription(&name, Some(&description))
    }?;

    match symbol_style(projection) {
        SymbolStyle::Template => {
            unsafe { image.setTemplate(true) };
            Some(image)
        }
        SymbolStyle::HierarchicalRed => {
            let configuration = unsafe {
                NSImageSymbolConfiguration::configurationWithHierarchicalColor(
                    &NSColor::systemRedColor(),
                )
            };
            let image =
                unsafe { image.imageWithSymbolConfiguration(&configuration) }.unwrap_or(image);
            unsafe { image.setTemplate(false) };
            Some(image)
        }
    }
}

fn set_recognition_pulse(button: &NSStatusBarButton, enabled: bool) {
    let key = NSString::from_str("ptt2me-recognition-pulse");
    button.setWantsLayer(true);

    let layer: *mut AnyObject = unsafe { msg_send![button, layer] };
    let Some(layer) = NonNull::new(layer) else {
        return;
    };

    if !enabled {
        unsafe {
            let _: () = msg_send![layer.as_ref(), removeAnimationForKey: &*key];
            button.setAlphaValue(1.0);
        }
        return;
    }

    let Some(animation_class) = AnyClass::get("CABasicAnimation") else {
        return;
    };
    let Some(number_class) = AnyClass::get("NSNumber") else {
        return;
    };

    let key_path = NSString::from_str("opacity");
    let animation: Retained<AnyObject> =
        unsafe { msg_send_id![animation_class, animationWithKeyPath: &*key_path] };
    let from_value: Retained<AnyObject> =
        unsafe { msg_send_id![number_class, numberWithDouble: 1.0f64] };
    let to_value: Retained<AnyObject> =
        unsafe { msg_send_id![number_class, numberWithDouble: 0.35f64] };

    unsafe {
        let _: () = msg_send![&*animation, setFromValue: &*from_value];
        let _: () = msg_send![&*animation, setToValue: &*to_value];
        let _: () = msg_send![&*animation, setDuration: 0.7f64];
        let _: () = msg_send![&*animation, setAutoreverses: true];
        let _: () = msg_send![&*animation, setRepeatCount: f32::INFINITY];
        let _: () = msg_send![layer.as_ref(), addAnimation: &*animation, forKey: &*key];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use crate::hotkey::{HotkeyControl, HotkeySignal, KeyboardObservation, ObservationKind};
    use crate::preferences::{HoldThreshold, Preferences, TriggerKey};
    use crate::state::PermissionKind;

    #[test]
    fn ready_recording_and_recognition_have_exact_presentations() {
        assert_eq!(
            MenuProjection::from_status(&AppStatus::Ready),
            MenuProjection {
                title: "● Готово".into(),
                symbol: "mic",
                pulse: false,
                style: SymbolStyle::Template,
            }
        );
        assert_eq!(
            MenuProjection::from_status(&AppStatus::Recording),
            MenuProjection {
                title: "● Запись…".into(),
                symbol: "record.circle.fill",
                pulse: false,
                style: SymbolStyle::HierarchicalRed,
            }
        );
        assert_eq!(
            MenuProjection::from_status(&AppStatus::Recognizing),
            MenuProjection {
                title: "● Распознавание…".into(),
                symbol: "waveform",
                pulse: true,
                style: SymbolStyle::Template,
            }
        );
    }

    #[test]
    fn startup_permission_and_error_have_exact_presentations() {
        assert_eq!(
            MenuProjection::from_status(&AppStatus::Starting),
            MenuProjection {
                title: "● Запуск…".into(),
                symbol: "hourglass",
                pulse: false,
                style: SymbolStyle::Template,
            }
        );
        assert_eq!(
            MenuProjection::from_status(&AppStatus::PermissionBlocked(PermissionKind::Microphone)),
            MenuProjection {
                title: "● Нужен доступ: микрофон".into(),
                symbol: "exclamationmark.triangle.fill",
                pulse: false,
                style: SymbolStyle::HierarchicalRed,
            }
        );
        assert_eq!(
            MenuProjection::from_status(&AppStatus::PermissionBlocked(
                PermissionKind::InputMonitoring
            )),
            MenuProjection {
                title: "● Нужен доступ: мониторинг ввода".into(),
                symbol: "exclamationmark.triangle.fill",
                pulse: false,
                style: SymbolStyle::HierarchicalRed,
            }
        );
        assert_eq!(
            MenuProjection::from_status(&AppStatus::PermissionBlocked(
                PermissionKind::Accessibility
            )),
            MenuProjection {
                title: "● Нужен доступ: универсальный доступ".into(),
                symbol: "exclamationmark.triangle.fill",
                pulse: false,
                style: SymbolStyle::HierarchicalRed,
            }
        );
        assert_eq!(
            MenuProjection::from_status(&AppStatus::Error {
                message: "Ошибка микрофона",
                recoverable: true,
            }),
            MenuProjection {
                title: "● Ошибка: Ошибка микрофона".into(),
                symbol: "exclamationmark.circle.fill",
                pulse: false,
                style: SymbolStyle::Template,
            }
        );
    }

    #[test]
    fn blocking_states_use_red_warning_triangles() {
        let permission =
            MenuProjection::from_status(&AppStatus::PermissionBlocked(PermissionKind::Microphone));
        assert_eq!(permission.symbol, "exclamationmark.triangle.fill");
        assert_eq!(symbol_style(&permission), SymbolStyle::HierarchicalRed);

        let persistent = MenuProjection::from_status(&AppStatus::Error {
            message: "Модель недоступна",
            recoverable: false,
        });
        assert_eq!(persistent.symbol, "exclamationmark.triangle.fill");
        assert_eq!(symbol_style(&persistent), SymbolStyle::HierarchicalRed);
    }

    #[test]
    fn recoverable_error_keeps_template_circle() {
        let transient = MenuProjection::from_status(&AppStatus::Error {
            message: "Ошибка микрофона",
            recoverable: true,
        });
        assert_eq!(transient.symbol, "exclamationmark.circle.fill");
        assert_eq!(symbol_style(&transient), SymbolStyle::Template);
    }

    #[test]
    fn menu_descriptor_has_adjacent_trigger_and_threshold_controls() {
        assert_eq!(
            MENU_DESCRIPTOR,
            [
                MenuEntry::Status,
                MenuEntry::Version,
                MenuEntry::Trigger,
                MenuEntry::Threshold,
                MenuEntry::Separator,
                MenuEntry::Quit,
            ]
        );
    }

    #[test]
    fn preference_projection_marks_only_the_selected_threshold() {
        let projection = PreferenceProjection::from(Preferences {
            trigger: TriggerKey::KeyCode(54),
            threshold: HoldThreshold::MS_750,
        });
        assert_eq!(projection.trigger_title, "Правый Command");
        assert_eq!(projection.threshold_states, [false, false, true]);
    }

    fn observation(kind: ObservationKind, keycode: u16) -> KeyboardObservation {
        KeyboardObservation {
            kind,
            keycode,
            flags: 0,
            autorepeat: false,
            replay_marker: false,
        }
    }

    #[test]
    fn threshold_command_reaches_gate_before_runtime_drain() {
        let (sender, receiver) = mpsc::channel();
        let control = HotkeyControl::new(Preferences::default());
        let publisher = MenuCommandPublisher::new(sender, control.clone());
        let start = Instant::now();

        assert!(publisher.send(MenuCommand::SetThreshold(HoldThreshold::MS_750)));
        assert_eq!(
            control.observe_for_test(observation(ObservationKind::KeyDown, 63), start),
            Some(HotkeySignal::Pressed)
        );
        assert_eq!(
            control.observe_for_test(
                observation(ObservationKind::KeyUp, 63),
                start + Duration::from_millis(500),
            ),
            Some(HotkeySignal::Released { short: true })
        );
        assert_eq!(
            receiver.try_recv(),
            Ok(MenuCommand::SetThreshold(HoldThreshold::MS_750))
        );
    }

    #[test]
    fn reset_trigger_command_reaches_gate_before_runtime_drain() {
        let (sender, receiver) = mpsc::channel();
        let control = HotkeyControl::new(Preferences {
            trigger: TriggerKey::KeyCode(54),
            threshold: HoldThreshold::MS_500,
        });
        let publisher = MenuCommandPublisher::new(sender, control.clone());

        assert!(publisher.send(MenuCommand::ResetTrigger));
        assert_eq!(
            control.observe_for_test(observation(ObservationKind::KeyDown, 63), Instant::now(),),
            Some(HotkeySignal::Pressed)
        );
        assert_eq!(receiver.try_recv(), Ok(MenuCommand::ResetTrigger));
    }

    #[test]
    fn assignment_command_arms_gate_before_runtime_drain() {
        let (sender, receiver) = mpsc::channel();
        let control = HotkeyControl::new(Preferences::default());
        let publisher = MenuCommandPublisher::new(sender, control.clone());

        assert!(publisher.send(MenuCommand::BeginTriggerAssignment));
        assert!(matches!(
            control.observe_for_test(observation(ObservationKind::KeyDown, 49), Instant::now(),),
            Some(HotkeySignal::AssignmentSelected {
                trigger: TriggerKey::KeyCode(49),
                ..
            })
        ));
        assert_eq!(receiver.try_recv(), Ok(MenuCommand::BeginTriggerAssignment));
    }

    #[test]
    fn recording_uses_red_non_template_symbol_style() {
        assert_eq!(
            symbol_style(&MenuProjection::from_status(&AppStatus::Recording)),
            SymbolStyle::HierarchicalRed
        );
        assert_eq!(
            symbol_style(&MenuProjection::from_status(&AppStatus::Ready)),
            SymbolStyle::Template
        );
    }
}
