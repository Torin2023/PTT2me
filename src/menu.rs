use std::cell::Cell;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, Sender};

use objc2::{
    declare_class, msg_send, msg_send_id, mutability, rc::Retained, runtime::AnyClass,
    runtime::AnyObject, sel, ClassType, DeclaredClass,
};
use objc2_app_kit::{
    NSColor, NSControlStateValueOff, NSControlStateValueOn, NSImage, NSImageSymbolConfiguration,
    NSMenu, NSMenuItem, NSStatusBar, NSStatusBarButton, NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{MainThreadMarker, NSObject, NSObjectProtocol, NSString};

use crate::hotkey::{AssignmentEpoch, HotkeyControl};
use crate::preferences::{HoldThreshold, Preferences, TriggerKey};
use crate::state::{AppStatus, PermissionKind};
use crate::updater::{ArtifactKind, RetryAction, UpdaterState};

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
pub(crate) enum UpdaterMenuAction {
    CheckForUpdates,
    DownloadUpdate,
    RetryUpdate,
    OpenDownloadedUpdate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UpdaterMenuProjection {
    information_title: String,
    information_visible: bool,
    action_title: String,
    action_visible: bool,
    action_enabled: bool,
    action: Option<UpdaterMenuAction>,
}

impl UpdaterMenuProjection {
    fn hidden() -> Self {
        Self {
            information_title: String::new(),
            information_visible: false,
            action_title: String::new(),
            action_visible: false,
            action_enabled: false,
            action: None,
        }
    }

    fn information(title: String) -> Self {
        Self {
            information_title: title,
            information_visible: true,
            ..Self::hidden()
        }
    }

    fn information_and_action(
        information_title: String,
        action_title: String,
        action: UpdaterMenuAction,
        action_enabled: bool,
    ) -> Self {
        Self {
            information_title,
            information_visible: true,
            action_title,
            action_visible: true,
            action_enabled,
            action: Some(action),
        }
    }

    fn action(title: String, action: UpdaterMenuAction) -> Self {
        Self {
            action_title: title,
            action_visible: true,
            action_enabled: true,
            action: Some(action),
            ..Self::hidden()
        }
    }

    fn from_state(state: Option<&UpdaterState>, open_enabled: bool) -> Self {
        let Some(state) = state else {
            return Self::hidden();
        };
        match state {
            UpdaterState::Idle => Self::action(
                "Проверить обновления…".to_owned(),
                UpdaterMenuAction::CheckForUpdates,
            ),
            UpdaterState::Checking { .. } => Self::information("Проверка обновлений…".to_owned()),
            UpdaterState::Current => Self::information_and_action(
                "Установлена актуальная версия".to_owned(),
                "Проверить снова…".to_owned(),
                UpdaterMenuAction::CheckForUpdates,
                true,
            ),
            UpdaterState::Available { release, artifact }
            | UpdaterState::DivergedLocal { release, artifact } => {
                let kind = match artifact.kind {
                    ArtifactKind::Update => "без модели",
                    ArtifactKind::Full => "полная версия",
                };
                Self::information_and_action(
                    format!("Доступно обновление {} ({kind})", release.version),
                    format!("Скачать обновление {}…", release.version),
                    UpdaterMenuAction::DownloadUpdate,
                    true,
                )
            }
            UpdaterState::RepairRequired { release, .. } => Self::information_and_action(
                format!(
                    "Нужна полная версия {} для восстановления модели",
                    release.version
                ),
                format!("Скачать полную версию {}…", release.version),
                UpdaterMenuAction::DownloadUpdate,
                true,
            ),
            UpdaterState::Incompatible {
                release,
                required_macos,
            } => Self::information_and_action(
                format!(
                    "Обновление {} требует macOS {}",
                    release.version, required_macos
                ),
                "Проверить снова…".to_owned(),
                UpdaterMenuAction::CheckForUpdates,
                true,
            ),
            UpdaterState::UnpublishedLocal => Self::information_and_action(
                "Локальная версия новее опубликованной".to_owned(),
                "Проверить снова…".to_owned(),
                UpdaterMenuAction::CheckForUpdates,
                true,
            ),
            UpdaterState::RecheckingModel { .. } => {
                Self::information("Проверка модели…".to_owned())
            }
            UpdaterState::Downloading { release, .. } => {
                Self::information(format!("Загрузка обновления {}…", release.version))
            }
            UpdaterState::ReadyToInstall { release, .. } => Self::information_and_action(
                format!("Обновление {} загружено", release.version),
                "Открыть DMG и выйти…".to_owned(),
                UpdaterMenuAction::OpenDownloadedUpdate,
                open_enabled,
            ),
            UpdaterState::Opening { release, .. } => {
                Self::information(format!("Открытие обновления {}…", release.version))
            }
            UpdaterState::Failed { failure, retry, .. } => {
                let mut projection = match retry {
                    RetryAction::ManualCheck => Self::information_and_action(
                        "Не удалось проверить обновления".to_owned(),
                        "Повторить проверку".to_owned(),
                        UpdaterMenuAction::RetryUpdate,
                        true,
                    ),
                    RetryAction::Download => Self::information_and_action(
                        "Не удалось загрузить обновление".to_owned(),
                        "Повторить загрузку".to_owned(),
                        UpdaterMenuAction::RetryUpdate,
                        true,
                    ),
                    RetryAction::ModelRecheck => Self::information_and_action(
                        "Не удалось проверить модель".to_owned(),
                        "Повторить".to_owned(),
                        UpdaterMenuAction::RetryUpdate,
                        true,
                    ),
                };
                if *failure == crate::updater::UpdateFailure::WorkerStopped {
                    projection.information_title =
                        "Обработчик обновлений остановился. Повторите попытку".to_owned();
                }
                projection
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StatusActionProjection {
    title: &'static str,
    visible: bool,
    enabled: bool,
    kind: Option<StatusActionKind>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusActionKind {
    Permission(PermissionKind),
    RetryModelPreparation,
    RetryPermissionMigration,
    RetryAsr,
}

impl StatusActionProjection {
    const fn from_status(status: &AppStatus) -> Self {
        match status {
            AppStatus::PermissionBlocked(permission) => Self {
                title: "Открыть настройки…",
                visible: true,
                enabled: true,
                kind: Some(StatusActionKind::Permission(*permission)),
            },
            AppStatus::ModelRepairRequired | AppStatus::ModelPreparationFailed => Self {
                title: "Повторить подготовку модели",
                visible: true,
                enabled: true,
                kind: Some(StatusActionKind::RetryModelPreparation),
            },
            AppStatus::AsrUnavailable => Self {
                title: "Повторить запуск распознавания",
                visible: true,
                enabled: true,
                kind: Some(StatusActionKind::RetryAsr),
            },
            AppStatus::PermissionResetFailed => Self {
                title: "Повторить сброс разрешений",
                visible: true,
                enabled: true,
                kind: Some(StatusActionKind::RetryPermissionMigration),
            },
            _ => Self {
                title: "Открыть настройки…",
                visible: false,
                enabled: false,
                kind: None,
            },
        }
    }
}

impl MenuProjection {
    pub fn from_status(status: &AppStatus) -> Self {
        let (title, symbol, pulse, style) = match status {
            AppStatus::AsrCleanupPending => (
                "● Ожидание остановки распознавания…".into(),
                "hourglass",
                false,
                SymbolStyle::Template,
            ),
            AppStatus::AsrUnavailable => (
                "● Распознавание недоступно".into(),
                "exclamationmark.triangle.fill",
                false,
                SymbolStyle::Template,
            ),
            AppStatus::PreparingModel => (
                "● Подготовка модели…".into(),
                "hourglass",
                false,
                SymbolStyle::Template,
            ),
            AppStatus::ModelRepairRequired => (
                "● Требуется восстановление модели".into(),
                "exclamationmark.triangle.fill",
                false,
                SymbolStyle::HierarchicalRed,
            ),
            AppStatus::ModelPreparationFailed => (
                "● Ошибка подготовки модели".into(),
                "exclamationmark.triangle.fill",
                false,
                SymbolStyle::HierarchicalRed,
            ),
            AppStatus::ResettingPermissions => (
                "● Сброс разрешений…".into(),
                "hourglass",
                false,
                SymbolStyle::Template,
            ),
            AppStatus::PermissionResetFailed => (
                "● Не удалось сбросить разрешения".into(),
                "exclamationmark.triangle.fill",
                false,
                SymbolStyle::HierarchicalRed,
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
    UpdaterInfo,
    UpdaterAction,
    PermissionSettings,
    Trigger,
    Threshold,
    TrailingSpace,
    Separator,
    Quit,
}

pub const MENU_DESCRIPTOR: [MenuEntry; 10] = [
    MenuEntry::Status,
    MenuEntry::Version,
    MenuEntry::UpdaterInfo,
    MenuEntry::UpdaterAction,
    MenuEntry::PermissionSettings,
    MenuEntry::Trigger,
    MenuEntry::Threshold,
    MenuEntry::TrailingSpace,
    MenuEntry::Separator,
    MenuEntry::Quit,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuCommand {
    BeginTriggerAssignment { epoch: AssignmentEpoch },
    ResetTrigger,
    SetThreshold(HoldThreshold),
    SetAppendSpace(bool),
}

fn toggled_append_space(selected: bool) -> (bool, MenuCommand) {
    let selected = !selected;
    (selected, MenuCommand::SetAppendSpace(selected))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MenuAction {
    Quit,
    OpenPermission(PermissionKind),
    RetryModelPreparation,
    RetryPermissionMigration,
    RetryAsr,
}

#[derive(Clone)]
pub struct MenuReadiness {
    ready: Rc<Cell<bool>>,
}

impl MenuReadiness {
    pub fn new(ready: bool) -> Self {
        Self {
            ready: Rc::new(Cell::new(ready)),
        }
    }

    pub fn set_ready(&self, ready: bool) {
        self.ready.set(ready);
    }

    fn is_ready(&self) -> bool {
        self.ready.get()
    }
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
    action_sender: Sender<MenuAction>,
    updater_action_sender: Sender<UpdaterMenuAction>,
    status_action: Cell<Option<StatusActionKind>>,
    updater_action: Cell<Option<UpdaterMenuAction>>,
    append_space: Cell<bool>,
}

#[derive(Clone)]
struct MenuCommandPublisher {
    sender: Sender<MenuCommand>,
    hotkey: HotkeyControl,
    readiness: MenuReadiness,
}

impl MenuCommandPublisher {
    fn new(sender: Sender<MenuCommand>, hotkey: HotkeyControl, readiness: MenuReadiness) -> Self {
        Self {
            sender,
            hotkey,
            readiness,
        }
    }

    fn send(&self, command: MenuCommand) -> bool {
        match command {
            MenuCommand::BeginTriggerAssignment { .. } => false,
            MenuCommand::ResetTrigger => {
                self.hotkey.set_trigger(TriggerKey::FnGlobe);
                self.sender.send(command).is_ok()
            }
            MenuCommand::SetThreshold(threshold) => {
                self.hotkey.set_threshold(threshold);
                self.sender.send(command).is_ok()
            }
            MenuCommand::SetAppendSpace(_) => self.sender.send(command).is_ok(),
        }
    }

    fn begin_assignment(&self) -> bool {
        if !self.readiness.is_ready() {
            return false;
        }
        let Some(epoch) = self.hotkey.begin_assignment() else {
            return false;
        };
        if self
            .sender
            .send(MenuCommand::BeginTriggerAssignment { epoch })
            .is_ok()
        {
            true
        } else {
            self.hotkey.cancel_assignment(epoch);
            false
        }
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
        fn quit(&self, _sender: &AnyObject) {
            let _ = self.ivars().action_sender.send(MenuAction::Quit);
        }

        #[method(performStatusAction:)]
        fn perform_status_action(&self, _sender: &AnyObject) {
            let action = match self.ivars().status_action.get() {
                Some(StatusActionKind::Permission(permission)) => {
                    MenuAction::OpenPermission(permission)
                }
                Some(StatusActionKind::RetryModelPreparation) => {
                    MenuAction::RetryModelPreparation
                }
                Some(StatusActionKind::RetryPermissionMigration) => {
                    MenuAction::RetryPermissionMigration
                }
                Some(StatusActionKind::RetryAsr) => MenuAction::RetryAsr,
                None => return,
            };
            let _ = self.ivars().action_sender.send(action);
        }

        #[method(performUpdaterAction:)]
        fn perform_updater_action(&self, _sender: &AnyObject) {
            let Some(action) = self.ivars().updater_action.get() else {
                return;
            };
            let _ = self.ivars().updater_action_sender.send(action);
        }

        #[method(assignTrigger:)]
        fn assign_trigger(&self, _sender: &AnyObject) {
            self.ivars().publisher.begin_assignment();
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

        #[method(toggleTrailingSpace:)]
        fn toggle_trailing_space(&self, _sender: &AnyObject) {
            let (selected, command) =
                toggled_append_space(self.ivars().append_space.get());
            self.ivars().append_space.set(selected);
            self.ivars().publisher.send(command);
        }
    }
);

impl MenuTarget {
    fn new(
        mtm: MainThreadMarker,
        sender: Sender<MenuCommand>,
        hotkey: HotkeyControl,
        readiness: MenuReadiness,
        action_sender: Sender<MenuAction>,
        updater_action_sender: Sender<UpdaterMenuAction>,
        append_space: bool,
    ) -> Retained<Self> {
        let this = mtm.alloc().set_ivars(MenuTargetIvars {
            publisher: MenuCommandPublisher::new(sender, hotkey, readiness),
            action_sender,
            updater_action_sender,
            status_action: Cell::new(None),
            updater_action: Cell::new(None),
            append_space: Cell::new(append_space),
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
    status_action_row: Retained<NSMenuItem>,
    updater_info_row: Retained<NSMenuItem>,
    updater_action_row: Retained<NSMenuItem>,
    current_trigger_row: Retained<NSMenuItem>,
    threshold_rows: [Retained<NSMenuItem>; 3],
    trailing_space_row: Retained<NSMenuItem>,
    button: Retained<NSStatusBarButton>,
    _target: Retained<MenuTarget>,
    action_receiver: Receiver<MenuAction>,
    updater_action_receiver: Receiver<UpdaterMenuAction>,
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
        append_space: bool,
        sender: Sender<MenuCommand>,
        hotkey: HotkeyControl,
        readiness: MenuReadiness,
    ) -> Self {
        let mtm = main_thread_marker();
        let (action_sender, action_receiver) = mpsc::channel();
        let (updater_action_sender, updater_action_receiver) = mpsc::channel();
        let target = MenuTarget::new(
            mtm,
            sender,
            hotkey,
            readiness,
            action_sender,
            updater_action_sender,
            append_space,
        );
        let menu = unsafe { NSMenu::initWithTitle(mtm.alloc(), &NSString::from_str("PTT2me")) };
        unsafe { menu.setAutoenablesItems(false) };

        let mut status_row = None;
        let mut status_action_row = None;
        let mut updater_info_row = None;
        let mut updater_action_row = None;
        let mut current_trigger_row = None;
        let mut threshold_rows = Vec::with_capacity(HoldThreshold::OPTIONS.len());
        let mut trailing_space_row = None;
        for entry in MENU_DESCRIPTOR {
            match entry {
                MenuEntry::Status => {
                    let item = menu_item(mtm, "● Подготовка модели…", None);
                    unsafe { item.setEnabled(false) };
                    menu.addItem(&item);
                    status_row = Some(item);
                }
                MenuEntry::Version => {
                    let item = menu_item(mtm, concat!("PTT2me ", env!("CARGO_PKG_VERSION")), None);
                    unsafe { item.setEnabled(false) };
                    menu.addItem(&item);
                }
                MenuEntry::UpdaterInfo => {
                    let item = menu_item(mtm, "", None);
                    unsafe {
                        item.setEnabled(false);
                        item.setHidden(true);
                    }
                    menu.addItem(&item);
                    updater_info_row = Some(item);
                }
                MenuEntry::UpdaterAction => {
                    let item = menu_item(mtm, "", Some(sel!(performUpdaterAction:)));
                    unsafe {
                        item.setTarget(Some(&target));
                        item.setEnabled(false);
                        item.setHidden(true);
                    }
                    menu.addItem(&item);
                    updater_action_row = Some(item);
                }
                MenuEntry::PermissionSettings => {
                    let item =
                        menu_item(mtm, "Открыть настройки…", Some(sel!(performStatusAction:)));
                    unsafe {
                        item.setTarget(Some(&target));
                        item.setEnabled(false);
                        item.setHidden(true);
                    }
                    menu.addItem(&item);
                    status_action_row = Some(item);
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
                MenuEntry::TrailingSpace => {
                    let item = menu_item(mtm, "Пробел в конце", Some(sel!(toggleTrailingSpace:)));
                    unsafe {
                        item.setTarget(Some(&target));
                        item.setEnabled(true);
                    }
                    menu.addItem(&item);
                    trailing_space_row = Some(item);
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
            status_action_row: status_action_row
                .expect("menu descriptor must contain the status action row"),
            updater_info_row: updater_info_row
                .expect("menu descriptor must contain the updater information row"),
            updater_action_row: updater_action_row
                .expect("menu descriptor must contain the updater action row"),
            current_trigger_row: current_trigger_row
                .expect("menu descriptor must contain the current trigger row"),
            threshold_rows: threshold_rows
                .try_into()
                .unwrap_or_else(|_| panic!("menu descriptor must contain three threshold rows")),
            trailing_space_row: trailing_space_row
                .expect("menu descriptor must contain the trailing-space row"),
            button,
            _target: target,
            action_receiver,
            updater_action_receiver,
            pulse_active: false,
        };
        menu_bar.render(&AppStatus::PreparingModel);
        menu_bar.render_updater(None, false);
        menu_bar.render_preferences(preferences);
        menu_bar.render_append_space(append_space);
        menu_bar
    }

    /// Updates only the already-created status row and status-item button.
    ///
    /// The menu descriptor is consumed only by [`Self::new`], so state changes
    /// cannot add, remove, or rebuild menu entries.
    pub fn render(&mut self, status: &AppStatus) {
        let _mtm = main_thread_marker();

        let projection = MenuProjection::from_status(status);
        let status_action = StatusActionProjection::from_status(status);
        unsafe {
            self.status_row
                .setTitle(&NSString::from_str(&projection.title));
            self.status_action_row
                .setTitle(&NSString::from_str(status_action.title));
            self.status_action_row.setHidden(!status_action.visible);
            self.status_action_row.setEnabled(status_action.enabled);
        }
        self._target.ivars().status_action.set(status_action.kind);

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

    pub(crate) fn render_updater(&self, state: Option<&UpdaterState>, open_enabled: bool) {
        let projection = UpdaterMenuProjection::from_state(state, open_enabled);
        unsafe {
            self.updater_info_row
                .setTitle(&NSString::from_str(&projection.information_title));
            self.updater_info_row
                .setHidden(!projection.information_visible);
            self.updater_action_row
                .setTitle(&NSString::from_str(&projection.action_title));
            self.updater_action_row
                .setHidden(!projection.action_visible);
            self.updater_action_row
                .setEnabled(projection.action_enabled);
        }
        self._target.ivars().updater_action.set(projection.action);
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

    pub fn render_append_space(&self, append_space: bool) {
        self._target.ivars().append_space.set(append_space);
        unsafe {
            self.trailing_space_row.setState(if append_space {
                NSControlStateValueOn
            } else {
                NSControlStateValueOff
            });
        }
    }

    pub(crate) fn take_action(&self) -> Option<MenuAction> {
        self.action_receiver.try_recv().ok()
    }

    pub(crate) fn take_updater_action(&self) -> Option<UpdaterMenuAction> {
        self.updater_action_receiver.try_recv().ok()
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
    use crate::update_manifest::{
        ArtifactDescriptor, MacOsVersion, RequiredModel, VerifiedRelease,
    };
    use crate::updater::{
        ArtifactKind, CheckReason, OfferDisposition, OperationId, RetryAction, SelectedArtifact,
        UpdateFailure, UpdaterState,
    };

    fn updater_release() -> Box<VerifiedRelease> {
        Box::new(VerifiedRelease {
            version: semver::Version::parse("1.0.6").unwrap(),
            build: 202608011200,
            source_commit: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            minimum_macos: MacOsVersion::parse("13.0").unwrap(),
            required_model: RequiredModel {
                id: "gigaam-v3-rnnt-v1".to_owned(),
                manifest_sha256:
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                        .to_owned(),
            },
            fresh_install: ArtifactDescriptor {
                url: "https://github.com/Torin2023/PTT2me/releases/download/v1.0.6/PTT2me-1.0.6-full-macos-arm64.dmg".to_owned(),
                sha256: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                    .to_owned(),
                size: 200,
            },
            application_update: ArtifactDescriptor {
                url: "https://github.com/Torin2023/PTT2me/releases/download/v1.0.6/PTT2me-1.0.6-update-macos-arm64.dmg".to_owned(),
                sha256: "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
                    .to_owned(),
                size: 20,
            },
            published_at: "2026-08-01T12:00:00Z".to_owned(),
        })
    }

    fn selected(kind: ArtifactKind, release: &VerifiedRelease) -> SelectedArtifact {
        SelectedArtifact {
            kind,
            descriptor: match kind {
                ArtifactKind::Full => release.fresh_install.clone(),
                ArtifactKind::Update => release.application_update.clone(),
            },
        }
    }

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
    fn unavailable_updater_is_hidden_while_idle_exposes_only_manual_check() {
        assert_eq!(
            UpdaterMenuProjection::from_state(None, true),
            UpdaterMenuProjection::hidden()
        );

        let idle = UpdaterMenuProjection::from_state(Some(&UpdaterState::Idle), true);
        assert!(!idle.information_visible);
        assert_eq!(idle.action_title, "Проверить обновления…");
        assert!(idle.action_visible);
        assert!(idle.action_enabled);
        assert_eq!(idle.action, Some(UpdaterMenuAction::CheckForUpdates));
    }

    #[test]
    fn updater_offer_projection_names_update_full_and_current_model_repair() {
        let release = updater_release();
        for (kind, expected) in [
            (
                ArtifactKind::Update,
                "Доступно обновление 1.0.6 (без модели)",
            ),
            (
                ArtifactKind::Full,
                "Доступно обновление 1.0.6 (полная версия)",
            ),
        ] {
            let projection = UpdaterMenuProjection::from_state(
                Some(&UpdaterState::Available {
                    release: release.clone(),
                    artifact: selected(kind, &release),
                }),
                true,
            );
            assert_eq!(projection.information_title, expected);
            assert_eq!(projection.action_title, "Скачать обновление 1.0.6…");
            assert_eq!(projection.action, Some(UpdaterMenuAction::DownloadUpdate));
        }

        let repair = UpdaterMenuProjection::from_state(
            Some(&UpdaterState::RepairRequired {
                release: release.clone(),
                artifact: selected(ArtifactKind::Full, &release),
            }),
            true,
        );
        assert_eq!(
            repair.information_title,
            "Нужна полная версия 1.0.6 для восстановления модели"
        );
        assert_eq!(repair.action, Some(UpdaterMenuAction::DownloadUpdate));
    }

    #[test]
    fn updater_busy_and_ready_projection_never_exposes_the_wrong_action() {
        let release = updater_release();
        let checking = UpdaterMenuProjection::from_state(
            Some(&UpdaterState::Checking {
                reason: CheckReason::Manual,
                operation_id: OperationId(7),
            }),
            true,
        );
        assert_eq!(checking.information_title, "Проверка обновлений…");
        assert!(!checking.action_visible);

        let downloading = UpdaterMenuProjection::from_state(
            Some(&UpdaterState::Downloading {
                release: release.clone(),
                artifact: selected(ArtifactKind::Update, &release),
                disposition: OfferDisposition::Available,
                operation_id: OperationId(8),
            }),
            true,
        );
        assert_eq!(downloading.information_title, "Загрузка обновления 1.0.6…");
        assert!(!downloading.action_visible);

        let ready_state = UpdaterState::ReadyToInstall {
            release: release.clone(),
            artifact: selected(ArtifactKind::Update, &release),
            disposition: OfferDisposition::Available,
            path: "/tmp/PTT2me.dmg".into(),
        };
        let blocked = UpdaterMenuProjection::from_state(Some(&ready_state), false);
        assert_eq!(blocked.information_title, "Обновление 1.0.6 загружено");
        assert_eq!(blocked.action_title, "Открыть DMG и выйти…");
        assert_eq!(
            blocked.action,
            Some(UpdaterMenuAction::OpenDownloadedUpdate)
        );
        assert!(!blocked.action_enabled);

        let allowed = UpdaterMenuProjection::from_state(Some(&ready_state), true);
        assert!(allowed.action_enabled);
    }

    #[test]
    fn updater_manual_failure_is_retryable_without_changing_voice_status() {
        let failure = UpdaterMenuProjection::from_state(
            Some(&UpdaterState::Failed {
                failure: UpdateFailure::Network,
                retry: RetryAction::ManualCheck,
                context: None,
            }),
            true,
        );
        assert_eq!(failure.information_title, "Не удалось проверить обновления");
        assert_eq!(failure.action_title, "Повторить проверку");
        assert_eq!(failure.action, Some(UpdaterMenuAction::RetryUpdate));
        assert!(failure.action_enabled);
    }

    #[test]
    fn startup_permission_and_error_have_exact_presentations() {
        assert_eq!(
            MenuProjection::from_status(&AppStatus::PreparingModel),
            MenuProjection {
                title: "● Подготовка модели…".into(),
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
    fn model_preparation_failures_expose_only_the_targeted_retry_action() {
        for status in [
            AppStatus::ModelRepairRequired,
            AppStatus::ModelPreparationFailed,
        ] {
            let action = StatusActionProjection::from_status(&status);
            assert_eq!(action.title, "Повторить подготовку модели");
            assert!(action.visible);
            assert!(action.enabled);
            assert_eq!(action.kind, Some(StatusActionKind::RetryModelPreparation));
        }
    }

    #[test]
    fn permission_migration_has_factual_status_and_one_targeted_retry() {
        assert_eq!(
            MenuProjection::from_status(&AppStatus::ResettingPermissions),
            MenuProjection {
                title: "● Сброс разрешений…".into(),
                symbol: "hourglass",
                pulse: false,
                style: SymbolStyle::Template,
            }
        );
        assert_eq!(
            MenuProjection::from_status(&AppStatus::PermissionResetFailed),
            MenuProjection {
                title: "● Не удалось сбросить разрешения".into(),
                symbol: "exclamationmark.triangle.fill",
                pulse: false,
                style: SymbolStyle::HierarchicalRed,
            }
        );
        assert_eq!(
            StatusActionProjection::from_status(&AppStatus::PermissionResetFailed),
            StatusActionProjection {
                title: "Повторить сброс разрешений",
                visible: true,
                enabled: true,
                kind: Some(StatusActionKind::RetryPermissionMigration),
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
                MenuEntry::UpdaterInfo,
                MenuEntry::UpdaterAction,
                MenuEntry::PermissionSettings,
                MenuEntry::Trigger,
                MenuEntry::Threshold,
                MenuEntry::TrailingSpace,
                MenuEntry::Separator,
                MenuEntry::Quit,
            ]
        );
    }

    #[test]
    fn trailing_space_toggle_emits_the_new_selected_value() {
        assert_eq!(
            toggled_append_space(false),
            (true, MenuCommand::SetAppendSpace(true))
        );
        assert_eq!(
            toggled_append_space(true),
            (false, MenuCommand::SetAppendSpace(false))
        );
    }

    #[test]
    fn trailing_space_command_reports_a_closed_runtime_channel() {
        let (sender, receiver) = mpsc::channel();
        drop(receiver);
        let publisher = MenuCommandPublisher::new(
            sender,
            HotkeyControl::new(Preferences::default()),
            MenuReadiness::new(true),
        );

        assert!(!publisher.send(MenuCommand::SetAppendSpace(true)));
    }

    #[test]
    fn permission_action_tracks_the_current_missing_permission() {
        assert_eq!(
            StatusActionProjection::from_status(&AppStatus::Ready),
            StatusActionProjection {
                title: "Открыть настройки…",
                visible: false,
                enabled: false,
                kind: None,
            }
        );
        for permission in [
            PermissionKind::Accessibility,
            PermissionKind::InputMonitoring,
            PermissionKind::Microphone,
        ] {
            assert_eq!(
                StatusActionProjection::from_status(&AppStatus::PermissionBlocked(permission)),
                StatusActionProjection {
                    title: "Открыть настройки…",
                    visible: true,
                    enabled: true,
                    kind: Some(StatusActionKind::Permission(permission)),
                }
            );
        }
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
        let publisher =
            MenuCommandPublisher::new(sender, control.clone(), MenuReadiness::new(true));
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
        let publisher =
            MenuCommandPublisher::new(sender, control.clone(), MenuReadiness::new(true));

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
        let publisher =
            MenuCommandPublisher::new(sender, control.clone(), MenuReadiness::new(true));

        assert!(publisher.begin_assignment());
        let selected_epoch = match control
            .observe_for_test(observation(ObservationKind::KeyDown, 49), Instant::now())
        {
            Some(HotkeySignal::AssignmentSelected {
                trigger: TriggerKey::KeyCode(49),
                epoch,
            }) => epoch,
            other => panic!("expected selected assignment, got {other:?}"),
        };
        assert_eq!(
            receiver.try_recv(),
            Ok(MenuCommand::BeginTriggerAssignment {
                epoch: selected_epoch,
            })
        );
    }

    #[test]
    fn non_ready_assignment_command_does_not_consume_the_next_key() {
        let (sender, receiver) = mpsc::channel();
        let control = HotkeyControl::new(Preferences::default());
        let readiness = MenuReadiness::new(false);
        let publisher = MenuCommandPublisher::new(sender, control.clone(), readiness);

        assert!(!publisher.begin_assignment());
        assert_eq!(
            control.observe_for_test(observation(ObservationKind::KeyDown, 49), Instant::now(),),
            None
        );
        assert_eq!(
            receiver.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        );
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

#[cfg(test)]
mod asr_tests {
    use super::*;
    #[test]
    fn unavailable_asr_offers_targeted_retry_only() {
        let action = StatusActionProjection::from_status(&AppStatus::AsrUnavailable);
        assert_eq!(action.kind, Some(StatusActionKind::RetryAsr));
        assert!(action.visible && action.enabled);
        assert!(!StatusActionProjection::from_status(&AppStatus::PreparingModel).visible);
    }
}
