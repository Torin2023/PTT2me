use std::cell::Cell;
use std::ptr::NonNull;
use std::sync::mpsc::{self, Receiver, Sender};

use objc2::{
    declare_class, msg_send, msg_send_id, mutability, rc::Retained, runtime::AnyClass,
    runtime::AnyObject, sel, ClassType, DeclaredClass,
};
use objc2_app_kit::{
    NSApplication, NSColor, NSImage, NSImageSymbolConfiguration, NSMenu, NSMenuItem, NSStatusBar,
    NSStatusBarButton, NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{MainThreadMarker, NSObject, NSObjectProtocol, NSString};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PermissionActionProjection {
    visible: bool,
    enabled: bool,
    permission: Option<PermissionKind>,
}

impl PermissionActionProjection {
    const fn from_status(status: &AppStatus) -> Self {
        match status {
            AppStatus::PermissionBlocked(permission) => Self {
                visible: true,
                enabled: true,
                permission: Some(*permission),
            },
            _ => Self {
                visible: false,
                enabled: false,
                permission: None,
            },
        }
    }
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
    PermissionSettings,
    Separator,
    Quit,
}

pub const MENU_DESCRIPTOR: [MenuEntry; 5] = [
    MenuEntry::Status,
    MenuEntry::Version,
    MenuEntry::PermissionSettings,
    MenuEntry::Separator,
    MenuEntry::Quit,
];

fn symbol_style(projection: &MenuProjection) -> SymbolStyle {
    projection.style
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MenuAction {
    OpenPermission(PermissionKind),
}

struct MenuTargetIvars {
    action_sender: Sender<MenuAction>,
    permission: Cell<Option<PermissionKind>>,
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

        #[method(openPermissionSettings:)]
        fn open_permission_settings(&self, _sender: &AnyObject) {
            if let Some(permission) = self.ivars().permission.get() {
                let _ = self
                    .ivars()
                    .action_sender
                    .send(MenuAction::OpenPermission(permission));
            }
        }
    }
);

impl MenuTarget {
    fn new(mtm: MainThreadMarker, action_sender: Sender<MenuAction>) -> Retained<Self> {
        let this = mtm.alloc().set_ivars(MenuTargetIvars {
            action_sender,
            permission: Cell::new(None),
        });
        unsafe { msg_send_id![super(this), init] }
    }
}

/// Owns the permanent four-row status menu and projects application state onto
/// the existing status row and status-bar button.
pub struct MenuBar {
    _status_bar: Retained<NSStatusBar>,
    _status_item: Retained<NSStatusItem>,
    _menu: Retained<NSMenu>,
    status_row: Retained<NSMenuItem>,
    permission_row: Retained<NSMenuItem>,
    button: Retained<NSStatusBarButton>,
    _target: Retained<MenuTarget>,
    action_receiver: Receiver<MenuAction>,
    pulse_active: bool,
}

impl MenuBar {
    /// Creates the status item and its immutable menu.
    ///
    /// # Panics
    ///
    /// Panics when called outside the AppKit main thread or if AppKit does not
    /// provide a button for its newly-created status item.
    pub fn new() -> Self {
        let mtm = main_thread_marker();
        let (action_sender, action_receiver) = mpsc::channel();
        let target = MenuTarget::new(mtm, action_sender);
        let menu = unsafe { NSMenu::initWithTitle(mtm.alloc(), &NSString::from_str("PTT2me")) };
        unsafe { menu.setAutoenablesItems(false) };

        let mut status_row = None;
        let mut permission_row = None;
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
                MenuEntry::PermissionSettings => {
                    let item = menu_item(
                        mtm,
                        "Открыть настройки…",
                        Some(sel!(openPermissionSettings:)),
                    );
                    unsafe {
                        item.setTarget(Some(&target));
                        item.setEnabled(false);
                        item.setHidden(true);
                    }
                    menu.addItem(&item);
                    permission_row = Some(item);
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
            permission_row: permission_row
                .expect("menu descriptor must contain the permission action row"),
            button,
            _target: target,
            action_receiver,
            pulse_active: false,
        };
        menu_bar.render(&AppStatus::Starting);
        menu_bar
    }

    /// Updates only the already-created status row and status-item button.
    ///
    /// The menu descriptor is consumed only by [`Self::new`], so state changes
    /// cannot add, remove, or rebuild menu entries.
    pub fn render(&mut self, status: &AppStatus) {
        let _mtm = main_thread_marker();

        let projection = MenuProjection::from_status(status);
        let permission_action = PermissionActionProjection::from_status(status);
        unsafe {
            self.status_row
                .setTitle(&NSString::from_str(&projection.title));
            self.permission_row.setHidden(!permission_action.visible);
            self.permission_row.setEnabled(permission_action.enabled);
        }
        self._target
            .ivars()
            .permission
            .set(permission_action.permission);

        if let Some(image) = system_symbol(&projection) {
            unsafe { self.button.setImage(Some(&image)) };
        }

        if self.pulse_active != projection.pulse {
            set_recognition_pulse(&self.button, projection.pulse);
            self.pulse_active = projection.pulse;
        }
    }

    pub(crate) fn take_action(&self) -> Option<MenuAction> {
        self.action_receiver.try_recv().ok()
    }
}

impl Default for MenuBar {
    fn default() -> Self {
        Self::new()
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
    fn menu_descriptor_contains_the_five_approved_entries() {
        assert_eq!(
            MENU_DESCRIPTOR,
            [
                MenuEntry::Status,
                MenuEntry::Version,
                MenuEntry::PermissionSettings,
                MenuEntry::Separator,
                MenuEntry::Quit,
            ]
        );
    }

    #[test]
    fn permission_action_tracks_the_current_missing_permission() {
        assert_eq!(
            PermissionActionProjection::from_status(&AppStatus::Ready),
            PermissionActionProjection {
                visible: false,
                enabled: false,
                permission: None,
            }
        );
        for permission in [
            PermissionKind::Accessibility,
            PermissionKind::InputMonitoring,
            PermissionKind::Microphone,
        ] {
            assert_eq!(
                PermissionActionProjection::from_status(&AppStatus::PermissionBlocked(permission)),
                PermissionActionProjection {
                    visible: true,
                    enabled: true,
                    permission: Some(permission),
                }
            );
        }
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
