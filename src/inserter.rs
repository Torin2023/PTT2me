use std::time::{Duration, Instant};
use std::{error::Error, fmt};

use core_graphics::{
    event::{CGEvent, CGEventFlags, CGEventTapLocation},
    event_source::{CGEventSource, CGEventSourceStateID},
};
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_app_kit::{NSPasteboard, NSPasteboardItem, NSPasteboardTypeString, NSPasteboardWriting};
use objc2_foundation::{NSArray, NSData, NSString};

const PASTE_KEYCODE: u16 = 9;
pub(crate) const PASTEBOARD_SETTLE_DELAY_MS: u64 = 30;
pub(crate) const PASTEBOARD_RESTORE_DELAY_MS: u64 = 1_000;

#[derive(Debug, Clone, Copy)]
struct SnapshotLimits {
    max_items: usize,
    max_representations_per_item: usize,
    max_representations: usize,
    max_type_utf16_units: usize,
    max_representation_bytes: usize,
    max_payload_bytes: usize,
    deadline: Duration,
}

impl Default for SnapshotLimits {
    fn default() -> Self {
        Self {
            max_items: 64,
            max_representations_per_item: 32,
            max_representations: 256,
            max_type_utf16_units: 1_024,
            max_representation_bytes: 64 * 1024 * 1024,
            max_payload_bytes: 128 * 1024 * 1024,
            deadline: Duration::from_millis(500),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Default)]
struct PasteboardSnapshot {
    items: Vec<PasteboardItemSnapshot>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct PasteboardItemSnapshot {
    representations: Vec<PasteboardRepresentation>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct PasteboardRepresentation {
    type_name: String,
    data: Vec<u8>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct SnapshotCapture {
    snapshot: PasteboardSnapshot,
    change_count: isize,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct TemporaryWrite {
    change_count: isize,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct TemporaryWriteFailure {
    error: InsertError,
    change_count: Option<isize>,
}

trait PasteboardAccess {
    fn change_count(&self) -> isize;
    fn snapshot(&mut self) -> Result<SnapshotCapture, InsertError>;
    fn write_temporary_text(
        &mut self,
        text: &str,
        expected_change_count: isize,
    ) -> Result<TemporaryWrite, TemporaryWriteFailure>;
    fn restore(
        &mut self,
        snapshot: &PasteboardSnapshot,
        expected_change_count: isize,
    ) -> Result<(), InsertError>;
}

trait PasteCommand {
    fn send_command_v(&mut self) -> Result<(), InsertError>;
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct AccessibilityFailure {
    stage: &'static str,
    attribute: Option<&'static str>,
    error_code: Option<i32>,
}

impl AccessibilityFailure {
    const fn new(
        stage: &'static str,
        attribute: Option<&'static str>,
        error_code: Option<i32>,
    ) -> Self {
        Self {
            stage,
            attribute,
            error_code,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum InsertError {
    EmptyText,
    SecureField,
    Accessibility(AccessibilityFailure),
    PasteboardSnapshot,
    PasteboardWrite,
    PasteboardChanged,
    PasteboardRestore,
    EventSource,
    KeyboardEvent,
}

impl InsertError {
    pub(crate) const fn accessibility(
        stage: &'static str,
        attribute: Option<&'static str>,
        error_code: Option<i32>,
    ) -> Self {
        Self::Accessibility(AccessibilityFailure::new(stage, attribute, error_code))
    }

    pub(crate) const fn kind(self) -> &'static str {
        match self {
            Self::EmptyText => "empty_text",
            Self::SecureField => "secure_field",
            Self::Accessibility(_) => "accessibility",
            Self::PasteboardSnapshot => "pasteboard_snapshot",
            Self::PasteboardWrite => "pasteboard_write",
            Self::PasteboardChanged => "pasteboard_changed",
            Self::PasteboardRestore => "pasteboard_restore",
            Self::EventSource => "event_source",
            Self::KeyboardEvent => "keyboard_event",
        }
    }

    pub(crate) const fn diagnostic_stage(self) -> Option<&'static str> {
        match self {
            Self::Accessibility(failure) => Some(failure.stage),
            _ => None,
        }
    }

    pub(crate) const fn ax_attribute(self) -> Option<&'static str> {
        match self {
            Self::Accessibility(failure) => failure.attribute,
            _ => None,
        }
    }

    pub(crate) const fn ax_error_code(self) -> Option<i32> {
        match self {
            Self::Accessibility(failure) => failure.error_code,
            _ => None,
        }
    }
}

impl fmt::Display for InsertError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyText => formatter.write_str("cannot insert empty text"),
            Self::SecureField => formatter.write_str("cannot insert into a secure text field"),
            Self::Accessibility(failure) => write!(
                formatter,
                "could not inspect the focused field through Accessibility (stage={}, attribute={:?}, ax_error={:?})",
                failure.stage,
                failure.attribute,
                failure.error_code
            ),
            Self::PasteboardSnapshot => formatter.write_str("could not snapshot the pasteboard"),
            Self::PasteboardWrite => formatter.write_str("could not write to the pasteboard"),
            Self::PasteboardChanged => formatter.write_str("pasteboard changed before insertion"),
            Self::PasteboardRestore => formatter.write_str("could not restore the pasteboard"),
            Self::EventSource => formatter.write_str("could not create a keyboard event source"),
            Self::KeyboardEvent => formatter.write_str("could not create a paste keyboard event"),
        }
    }
}

impl Error for InsertError {}

/// Removes outer whitespace and optionally appends one separator space without
/// otherwise modifying recognised text.
pub fn normalize_text(text: &str, append_space: bool) -> Option<String> {
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| {
        if append_space {
            format!("{trimmed} ")
        } else {
            trimmed.to_owned()
        }
    })
}

struct SystemPasteboard {
    pasteboard: Retained<NSPasteboard>,
}

trait SnapshotClock {
    fn elapsed(&self) -> Duration;
}

struct SystemSnapshotClock {
    started: Instant,
}

impl SystemSnapshotClock {
    fn new() -> Self {
        Self {
            started: Instant::now(),
        }
    }
}

impl SnapshotClock for SystemSnapshotClock {
    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }
}

struct SnapshotBudget {
    limits: SnapshotLimits,
    representations: usize,
    payload_bytes: usize,
}

impl SnapshotBudget {
    fn new(limits: SnapshotLimits) -> Self {
        Self {
            limits,
            representations: 0,
            payload_bytes: 0,
        }
    }

    fn check_deadline<C: SnapshotClock>(&self, clock: &C) -> Result<(), InsertError> {
        let elapsed = clock.elapsed();
        if elapsed < self.limits.deadline {
            Ok(())
        } else {
            tracing::warn!(
                error_category = "pasteboard_snapshot_deadline",
                elapsed_ms = elapsed.as_millis() as u64,
                limit_ms = self.limits.deadline.as_millis() as u64,
            );
            Err(InsertError::PasteboardSnapshot)
        }
    }

    fn admit_item_count(&self, count: usize) -> Result<(), InsertError> {
        check_snapshot_limit("items", count, self.limits.max_items)
    }

    fn admit_representations(&mut self, count: usize) -> Result<(), InsertError> {
        check_snapshot_limit(
            "representations_per_item",
            count,
            self.limits.max_representations_per_item,
        )?;
        let total = self
            .representations
            .checked_add(count)
            .ok_or_else(|| snapshot_counter_overflow("representations"))?;
        check_snapshot_limit("representations", total, self.limits.max_representations)?;
        self.representations = total;
        Ok(())
    }

    fn admit_type_units(&self, units: usize) -> Result<(), InsertError> {
        check_snapshot_limit(
            "type_name_utf16_units",
            units,
            self.limits.max_type_utf16_units,
        )
    }

    fn admit_payload(&mut self, bytes: usize) -> Result<(), InsertError> {
        check_snapshot_limit(
            "representation_bytes",
            bytes,
            self.limits.max_representation_bytes,
        )?;
        let total = self
            .payload_bytes
            .checked_add(bytes)
            .ok_or_else(|| snapshot_counter_overflow("payload_bytes"))?;
        check_snapshot_limit("payload_bytes", total, self.limits.max_payload_bytes)?;
        self.payload_bytes = total;
        Ok(())
    }
}

fn check_snapshot_limit(
    resource: &'static str,
    observed: usize,
    limit: usize,
) -> Result<(), InsertError> {
    if observed <= limit {
        Ok(())
    } else {
        tracing::warn!(
            error_category = "pasteboard_snapshot_limit",
            resource,
            observed,
            limit,
        );
        Err(InsertError::PasteboardSnapshot)
    }
}

fn snapshot_counter_overflow(resource: &'static str) -> InsertError {
    tracing::warn!(
        error_category = "pasteboard_snapshot_counter_overflow",
        resource,
    );
    InsertError::PasteboardSnapshot
}

trait SnapshotReader {
    type TypeName;
    type Data;

    fn item_count(&mut self) -> Result<usize, InsertError>;
    fn representation_count(&mut self, item: usize) -> Result<usize, InsertError>;
    fn type_name(
        &mut self,
        item: usize,
        representation: usize,
    ) -> Result<Self::TypeName, InsertError>;
    fn type_name_utf16_units(&self, type_name: &Self::TypeName) -> usize;
    fn type_name_to_string(&mut self, type_name: &Self::TypeName) -> Result<String, InsertError>;
    fn representation_data(
        &mut self,
        type_name: &Self::TypeName,
    ) -> Result<Self::Data, InsertError>;
    fn data_len(&self, data: &Self::Data) -> usize;
    fn copy_data(&mut self, data: Self::Data) -> Result<Vec<u8>, InsertError>;
}

fn capture_snapshot<R: SnapshotReader, C: SnapshotClock>(
    reader: &mut R,
    limits: SnapshotLimits,
    clock: &C,
) -> Result<PasteboardSnapshot, InsertError> {
    let mut budget = SnapshotBudget::new(limits);
    budget.check_deadline(clock)?;
    let item_count = reader.item_count()?;
    budget.check_deadline(clock)?;
    budget.admit_item_count(item_count)?;
    let mut items = Vec::new();
    items
        .try_reserve_exact(item_count)
        .map_err(|_| InsertError::PasteboardSnapshot)?;

    for item_index in 0..item_count {
        budget.check_deadline(clock)?;
        let representation_count = reader.representation_count(item_index)?;
        budget.check_deadline(clock)?;
        budget.admit_representations(representation_count)?;
        let mut representations = Vec::new();
        representations
            .try_reserve_exact(representation_count)
            .map_err(|_| InsertError::PasteboardSnapshot)?;

        for representation_index in 0..representation_count {
            budget.check_deadline(clock)?;
            let type_name = reader.type_name(item_index, representation_index)?;
            budget.check_deadline(clock)?;
            let type_units = reader.type_name_utf16_units(&type_name);
            budget.check_deadline(clock)?;
            budget.admit_type_units(type_units)?;
            let type_name_string = reader.type_name_to_string(&type_name)?;
            budget.check_deadline(clock)?;
            let data = reader.representation_data(&type_name)?;
            budget.check_deadline(clock)?;
            let data_len = reader.data_len(&data);
            budget.check_deadline(clock)?;
            budget.admit_payload(data_len)?;
            let copied = reader.copy_data(data)?;
            budget.check_deadline(clock)?;
            if copied.len() != data_len {
                return Err(InsertError::PasteboardSnapshot);
            }
            representations.push(PasteboardRepresentation {
                type_name: type_name_string,
                data: copied,
            });
        }
        items.push(PasteboardItemSnapshot { representations });
    }
    budget.check_deadline(clock)?;
    Ok(PasteboardSnapshot { items })
}

fn finish_snapshot_capture<C: SnapshotClock>(
    snapshot: PasteboardSnapshot,
    expected_change_count: isize,
    observed_change_count: isize,
    budget: &SnapshotBudget,
    clock: &C,
) -> Result<SnapshotCapture, InsertError> {
    if observed_change_count != expected_change_count {
        return Err(InsertError::PasteboardChanged);
    }
    budget.check_deadline(clock)?;
    Ok(SnapshotCapture {
        snapshot,
        change_count: expected_change_count,
    })
}

struct NativeSnapshotReader {
    items: Retained<NSArray<NSPasteboardItem>>,
}

struct NativeTypeName {
    item: usize,
    value: Retained<NSString>,
}

impl SnapshotReader for NativeSnapshotReader {
    type TypeName = NativeTypeName;
    type Data = Retained<NSData>;

    fn item_count(&mut self) -> Result<usize, InsertError> {
        Ok(self.items.len())
    }

    fn representation_count(&mut self, item: usize) -> Result<usize, InsertError> {
        let item = self
            .items
            .get(item)
            .ok_or(InsertError::PasteboardSnapshot)?;
        Ok(unsafe { item.types() }.len())
    }

    fn type_name(
        &mut self,
        item_index: usize,
        representation: usize,
    ) -> Result<Self::TypeName, InsertError> {
        let item = self
            .items
            .get(item_index)
            .ok_or(InsertError::PasteboardSnapshot)?;
        let types = unsafe { item.types() };
        let type_name = types
            .get(representation)
            .ok_or(InsertError::PasteboardSnapshot)?;
        let value = unsafe { Retained::retain(type_name as *const NSString as *mut NSString) }
            .ok_or(InsertError::PasteboardSnapshot)?;
        Ok(NativeTypeName {
            item: item_index,
            value,
        })
    }

    fn type_name_utf16_units(&self, type_name: &Self::TypeName) -> usize {
        type_name.value.len()
    }

    fn type_name_to_string(&mut self, type_name: &Self::TypeName) -> Result<String, InsertError> {
        Ok(type_name.value.to_string())
    }

    fn representation_data(
        &mut self,
        type_name: &Self::TypeName,
    ) -> Result<Self::Data, InsertError> {
        let item = self
            .items
            .get(type_name.item)
            .ok_or(InsertError::PasteboardSnapshot)?;
        unsafe { item.dataForType(&type_name.value) }.ok_or(InsertError::PasteboardSnapshot)
    }

    fn data_len(&self, data: &Self::Data) -> usize {
        data.len()
    }

    fn copy_data(&mut self, data: Self::Data) -> Result<Vec<u8>, InsertError> {
        let bytes = data.bytes();
        let mut copied = Vec::new();
        copied
            .try_reserve_exact(bytes.len())
            .map_err(|_| InsertError::PasteboardSnapshot)?;
        copied.extend_from_slice(bytes);
        Ok(copied)
    }
}

fn pasteboard_items_or_error<T>(items: Option<T>) -> Result<T, InsertError> {
    items.ok_or(InsertError::PasteboardSnapshot)
}

impl SystemPasteboard {
    fn new(pasteboard: Retained<NSPasteboard>) -> Self {
        Self { pasteboard }
    }
}

impl PasteboardAccess for SystemPasteboard {
    fn change_count(&self) -> isize {
        unsafe { self.pasteboard.changeCount() }
    }

    fn snapshot(&mut self) -> Result<SnapshotCapture, InsertError> {
        let clock = SystemSnapshotClock::new();
        let limits = SnapshotLimits::default();
        let budget = SnapshotBudget::new(limits);
        let change_count = self.change_count();
        budget.check_deadline(&clock)?;
        let items = pasteboard_items_or_error(unsafe { self.pasteboard.pasteboardItems() })?;
        let snapshot = capture_snapshot(&mut NativeSnapshotReader { items }, limits, &clock)?;
        let observed_change_count = self.change_count();
        finish_snapshot_capture(
            snapshot,
            change_count,
            observed_change_count,
            &budget,
            &clock,
        )
    }

    fn write_temporary_text(
        &mut self,
        text: &str,
        expected_change_count: isize,
    ) -> Result<TemporaryWrite, TemporaryWriteFailure> {
        let text = NSString::from_str(text);
        unsafe {
            if self.pasteboard.changeCount() != expected_change_count {
                return Err(TemporaryWriteFailure {
                    error: InsertError::PasteboardChanged,
                    change_count: None,
                });
            }
            self.pasteboard.clearContents();
            let written = self
                .pasteboard
                .setString_forType(&text, NSPasteboardTypeString);
            let change_count = self.pasteboard.changeCount();
            if written {
                Ok(TemporaryWrite { change_count })
            } else {
                Err(TemporaryWriteFailure {
                    error: InsertError::PasteboardWrite,
                    change_count: Some(change_count),
                })
            }
        }
    }

    fn restore(
        &mut self,
        snapshot: &PasteboardSnapshot,
        expected_change_count: isize,
    ) -> Result<(), InsertError> {
        let objects = reconstruct_items(snapshot)?;
        unsafe {
            if self.pasteboard.changeCount() != expected_change_count {
                return Ok(());
            }
            self.pasteboard.clearContents();
            if snapshot.items.is_empty() || self.pasteboard.writeObjects(&objects) {
                Ok(())
            } else {
                Err(InsertError::PasteboardRestore)
            }
        }
    }
}

struct SystemPasteCommand;

impl PasteCommand for SystemPasteCommand {
    fn send_command_v(&mut self) -> Result<(), InsertError> {
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .map_err(|_| InsertError::EventSource)?;
        post_command_v(source.clone())?;
        post_command_v_up(source)
    }
}

struct InsertionTransaction<P: PasteboardAccess, C> {
    pasteboard: P,
    command: C,
    snapshot: PasteboardSnapshot,
    temporary: TemporaryWrite,
    restore_required: bool,
}

pub(crate) struct PendingInsertion {
    inner: InsertionTransaction<SystemPasteboard, SystemPasteCommand>,
}

impl PendingInsertion {
    pub(crate) fn begin(text: &str) -> Result<Self, InsertError> {
        let pasteboard = unsafe { NSPasteboard::generalPasteboard() };
        InsertionTransaction::begin_with(
            text,
            SystemPasteboard::new(pasteboard),
            SystemPasteCommand,
        )
        .map(|inner| Self { inner })
    }

    pub(crate) fn paste(&mut self) -> Result<(), InsertError> {
        self.inner.paste()
    }

    pub(crate) fn restore(&mut self) -> Result<(), InsertError> {
        self.inner.restore()
    }

    pub(crate) fn restore_after_paste_failure(&mut self, primary: InsertError) -> InsertError {
        self.inner.restore_after_paste_failure(primary)
    }
}

impl<P: PasteboardAccess, C: PasteCommand> InsertionTransaction<P, C> {
    fn begin_with(text: &str, mut pasteboard: P, command: C) -> Result<Self, InsertError> {
        if text.is_empty() {
            return Err(InsertError::EmptyText);
        }
        let SnapshotCapture {
            snapshot,
            change_count,
        } = pasteboard.snapshot()?;
        let temporary = match pasteboard.write_temporary_text(text, change_count) {
            Ok(temporary) => temporary,
            Err(failure) => {
                if let Some(owned_change_count) = failure.change_count {
                    if pasteboard.restore(&snapshot, owned_change_count).is_err() {
                        tracing::warn!(
                            error_category = "pasteboard_restore_after_temporary_write_failure"
                        );
                    }
                }
                return Err(failure.error);
            }
        };
        Ok(Self {
            pasteboard,
            command,
            snapshot,
            temporary,
            restore_required: true,
        })
    }

    pub(crate) fn paste(&mut self) -> Result<(), InsertError> {
        // A copy made during the settle delay belongs to the user. Never
        // knowingly paste it as the result of this dictation transaction.
        if self.pasteboard.change_count() != self.temporary.change_count {
            return Err(InsertError::PasteboardChanged);
        }
        self.command.send_command_v()
    }

    pub(crate) fn restore(&mut self) -> Result<(), InsertError> {
        if !self.restore_required {
            return Ok(());
        }
        self.pasteboard
            .restore(&self.snapshot, self.temporary.change_count)?;
        self.restore_required = false;
        Ok(())
    }

    pub(crate) fn restore_after_paste_failure(&mut self, primary: InsertError) -> InsertError {
        if self.restore().is_err() {
            tracing::warn!(error_category = "pasteboard_restore_after_insert_failure");
        }
        primary
    }
}

impl<P: PasteboardAccess, C> Drop for InsertionTransaction<P, C> {
    fn drop(&mut self) {
        if self.restore_required
            && self
                .pasteboard
                .restore(&self.snapshot, self.temporary.change_count)
                .is_err()
        {
            tracing::warn!(error_category = "pasteboard_restore_on_drop");
        }
        self.restore_required = false;
    }
}

fn reconstruct_items(
    snapshot: &PasteboardSnapshot,
) -> Result<Retained<NSArray<ProtocolObject<dyn NSPasteboardWriting>>>, InsertError> {
    let mut objects = Vec::with_capacity(snapshot.items.len());
    for saved_item in &snapshot.items {
        let item = unsafe { NSPasteboardItem::new() };
        for saved in &saved_item.representations {
            let pasteboard_type = NSString::from_str(&saved.type_name);
            let data = NSData::with_bytes(&saved.data);
            if !unsafe { item.setData_forType(&data, &pasteboard_type) } {
                return Err(InsertError::PasteboardRestore);
            }
        }
        objects.push(ProtocolObject::from_retained(item));
    }
    Ok(NSArray::from_vec(objects))
}

fn post_command_v(source: CGEventSource) -> Result<(), InsertError> {
    let event = CGEvent::new_keyboard_event(source, PASTE_KEYCODE, true)
        .map_err(|_| InsertError::KeyboardEvent)?;
    event.set_flags(CGEventFlags::CGEventFlagCommand);
    event.post(CGEventTapLocation::AnnotatedSession);
    Ok(())
}

fn post_command_v_up(source: CGEventSource) -> Result<(), InsertError> {
    let event = CGEvent::new_keyboard_event(source, PASTE_KEYCODE, false)
        .map_err(|_| InsertError::KeyboardEvent)?;
    event.set_flags(CGEventFlags::CGEventFlagCommand);
    event.post(CGEventTapLocation::AnnotatedSession);
    Ok(())
}

/// Exercises the real NSPasteboard snapshot/restore path.
///
/// This entry point is available only to the custom main-thread test target.
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub fn run_pasteboard_main_thread_tests() -> Result<(), InsertError> {
    let pasteboard = unsafe { NSPasteboard::pasteboardWithUniqueName() };
    let expected = PasteboardSnapshot {
        items: vec![
            PasteboardItemSnapshot {
                representations: vec![
                    PasteboardRepresentation {
                        type_name: "public.utf8-plain-text".to_owned(),
                        data: b"plain".to_vec(),
                    },
                    PasteboardRepresentation {
                        type_name: "public.rtf".to_owned(),
                        data: b"{\\rtf1 rich}".to_vec(),
                    },
                ],
            },
            PasteboardItemSnapshot {
                representations: vec![PasteboardRepresentation {
                    type_name: "public.png".to_owned(),
                    data: vec![0x89, 0x50, 0x4e, 0x47],
                }],
            },
        ],
    };
    write_snapshot_fixture(&pasteboard, &expected)?;
    let mut system = SystemPasteboard::new(pasteboard);

    let captured = system.snapshot()?.snapshot;
    assert_snapshot_contains(&captured, &expected);
    let temporary = system
        .write_temporary_text("recognized", system.change_count())
        .map_err(|failure| failure.error)?;
    system.restore(&captured, temporary.change_count)?;
    assert_eq!(system.snapshot()?.snapshot, captured);

    let empty_pasteboard = unsafe { NSPasteboard::pasteboardWithUniqueName() };
    unsafe { empty_pasteboard.clearContents() };
    let mut empty_system = SystemPasteboard::new(empty_pasteboard);
    let empty_snapshot = empty_system.snapshot()?.snapshot;
    let temporary = empty_system
        .write_temporary_text("recognized", empty_system.change_count())
        .map_err(|failure| failure.error)?;
    empty_system.restore(&empty_snapshot, temporary.change_count)?;
    assert_eq!(
        empty_system.snapshot()?.snapshot,
        PasteboardSnapshot::default()
    );

    let oversized_pasteboard = unsafe { NSPasteboard::pasteboardWithUniqueName() };
    let oversized = PasteboardSnapshot {
        items: (0..65)
            .map(|index| PasteboardItemSnapshot {
                representations: vec![PasteboardRepresentation {
                    type_name: "public.utf8-plain-text".to_owned(),
                    data: vec![index as u8],
                }],
            })
            .collect(),
    };
    write_snapshot_fixture(&oversized_pasteboard, &oversized)?;
    let mut oversized_system = SystemPasteboard::new(oversized_pasteboard);
    let change_count = oversized_system.change_count();
    assert_eq!(
        oversized_system.snapshot(),
        Err(InsertError::PasteboardSnapshot)
    );
    assert_eq!(oversized_system.change_count(), change_count);
    let items =
        pasteboard_items_or_error(unsafe { oversized_system.pasteboard.pasteboardItems() })?;
    assert_eq!(items.len(), 65);
    for (index, expected) in [(0, 0_u8), (64, 64_u8)] {
        let item = items.get(index).ok_or(InsertError::PasteboardSnapshot)?;
        let data = unsafe { item.dataForType(NSPasteboardTypeString) }
            .ok_or(InsertError::PasteboardSnapshot)?;
        assert_eq!(data.bytes(), [expected]);
    }

    Ok(())
}

#[cfg(feature = "test-support")]
fn write_snapshot_fixture(
    pasteboard: &NSPasteboard,
    saved: &PasteboardSnapshot,
) -> Result<(), InsertError> {
    let objects = reconstruct_items(saved)?;
    unsafe {
        pasteboard.clearContents();
        if saved.items.is_empty() || pasteboard.writeObjects(&objects) {
            Ok(())
        } else {
            Err(InsertError::PasteboardWrite)
        }
    }
}

#[cfg(feature = "test-support")]
fn assert_snapshot_contains(actual: &PasteboardSnapshot, expected: &PasteboardSnapshot) {
    assert_eq!(actual.items.len(), expected.items.len());
    for (actual_item, expected_item) in actual.items.iter().zip(&expected.items) {
        for expected_representation in &expected_item.representations {
            assert!(
                actual_item
                    .representations
                    .contains(expected_representation),
                "missing representation: {}",
                expected_representation.type_name
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;
    use std::time::Duration;

    struct FakePasteboard {
        current: PasteboardSnapshot,
        change_count: isize,
        snapshot_error: Option<InsertError>,
        temporary_write_error: Option<InsertError>,
        restore_error: Option<InsertError>,
        replace_during_snapshot: Option<PasteboardSnapshot>,
        replace_before_temporary_write: Option<PasteboardSnapshot>,
        replace_before_ownership_check: Option<PasteboardSnapshot>,
        replace_during_restore: Option<PasteboardSnapshot>,
        temporary_write_calls: usize,
        restore_calls: usize,
        temporary_write_counter: Rc<Cell<usize>>,
        restore_counter: Rc<Cell<usize>>,
    }

    impl FakePasteboard {
        fn with_snapshot(current: PasteboardSnapshot) -> Self {
            Self {
                current,
                change_count: 0,
                snapshot_error: None,
                temporary_write_error: None,
                restore_error: None,
                replace_during_snapshot: None,
                replace_before_temporary_write: None,
                replace_before_ownership_check: None,
                replace_during_restore: None,
                temporary_write_calls: 0,
                restore_calls: 0,
                temporary_write_counter: Rc::new(Cell::new(0)),
                restore_counter: Rc::new(Cell::new(0)),
            }
        }
    }

    impl PasteboardAccess for FakePasteboard {
        fn change_count(&self) -> isize {
            self.change_count
        }

        fn snapshot(&mut self) -> Result<SnapshotCapture, InsertError> {
            if let Some(error) = self.snapshot_error {
                return Err(error);
            }
            let change_count = self.change_count;
            let snapshot = self.current.clone();
            if let Some(newer) = self.replace_during_snapshot.take() {
                self.current = newer;
                self.change_count += 1;
            }
            if self.change_count != change_count {
                return Err(InsertError::PasteboardChanged);
            }
            Ok(SnapshotCapture {
                snapshot,
                change_count,
            })
        }

        fn write_temporary_text(
            &mut self,
            text: &str,
            expected_change_count: isize,
        ) -> Result<TemporaryWrite, TemporaryWriteFailure> {
            if let Some(newer) = self.replace_before_temporary_write.take() {
                self.current = newer;
                self.change_count += 1;
            }
            if self.change_count != expected_change_count {
                return Err(TemporaryWriteFailure {
                    error: InsertError::PasteboardChanged,
                    change_count: None,
                });
            }
            self.temporary_write_calls += 1;
            self.temporary_write_counter
                .set(self.temporary_write_counter.get() + 1);
            self.change_count += 1;
            self.current = snapshot(&[&[("public.utf8-plain-text", text.as_bytes())]]);
            let result = TemporaryWrite {
                change_count: self.change_count,
            };
            match self.temporary_write_error {
                Some(error) => Err(TemporaryWriteFailure {
                    error,
                    change_count: Some(result.change_count),
                }),
                None => Ok(result),
            }
        }

        fn restore(
            &mut self,
            saved: &PasteboardSnapshot,
            expected_change_count: isize,
        ) -> Result<(), InsertError> {
            if let Some(newer) = self.replace_before_ownership_check.take() {
                self.current = newer;
                self.change_count += 1;
            }
            if let Some(newer) = self.replace_during_restore.take() {
                self.current = newer;
                self.change_count += 1;
            }
            if self.change_count != expected_change_count {
                return Ok(());
            }
            self.restore_calls += 1;
            self.restore_counter.set(self.restore_counter.get() + 1);
            if let Some(error) = self.restore_error {
                return Err(error);
            }
            self.current = saved.clone();
            self.change_count += 1;
            Ok(())
        }
    }

    struct FakePasteCommand {
        result: Result<(), InsertError>,
        calls: usize,
    }

    impl FakePasteCommand {
        fn succeed() -> Self {
            Self {
                result: Ok(()),
                calls: 0,
            }
        }

        fn fail(error: InsertError) -> Self {
            Self {
                result: Err(error),
                calls: 0,
            }
        }
    }

    impl PasteCommand for FakePasteCommand {
        fn send_command_v(&mut self) -> Result<(), InsertError> {
            self.calls += 1;
            self.result
        }
    }

    fn snapshot(items: &[&[(&str, &[u8])]]) -> PasteboardSnapshot {
        PasteboardSnapshot {
            items: items
                .iter()
                .map(|item| PasteboardItemSnapshot {
                    representations: item
                        .iter()
                        .map(|(type_name, data)| PasteboardRepresentation {
                            type_name: (*type_name).to_owned(),
                            data: data.to_vec(),
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    #[derive(Clone)]
    struct ManualSnapshotClock {
        elapsed: Rc<Cell<Duration>>,
    }

    impl ManualSnapshotClock {
        fn new() -> Self {
            Self {
                elapsed: Rc::new(Cell::new(Duration::ZERO)),
            }
        }

        fn advance(&self, duration: Duration) {
            self.elapsed.set(self.elapsed.get() + duration);
        }
    }

    impl SnapshotClock for ManualSnapshotClock {
        fn elapsed(&self) -> Duration {
            self.elapsed.get()
        }
    }

    #[derive(Clone)]
    struct FakeRepresentation {
        type_name: &'static str,
        type_units: usize,
        bytes: Option<Vec<u8>>,
        reported_len: usize,
        data_delay: Duration,
    }

    impl FakeRepresentation {
        fn bytes(type_name: &'static str, bytes: &[u8]) -> Self {
            Self {
                type_name,
                type_units: type_name.encode_utf16().count(),
                bytes: Some(bytes.to_vec()),
                reported_len: bytes.len(),
                data_delay: Duration::ZERO,
            }
        }
    }

    #[derive(Clone)]
    struct FakeTypeName {
        item: usize,
        representation: usize,
        value: &'static str,
        utf16_units: usize,
    }

    struct FakeData {
        bytes: Vec<u8>,
        reported_len: usize,
        copy_calls: Rc<Cell<usize>>,
    }

    struct FakeSnapshotReader {
        items: Vec<Vec<FakeRepresentation>>,
        item_count_override: Option<usize>,
        representation_count_override: Option<usize>,
        clock: ManualSnapshotClock,
        representation_count_calls: Rc<Cell<usize>>,
        type_calls: Rc<Cell<usize>>,
        data_calls: Rc<Cell<usize>>,
        copy_calls: Rc<Cell<usize>>,
    }

    impl FakeSnapshotReader {
        fn new(items: Vec<Vec<FakeRepresentation>>, clock: ManualSnapshotClock) -> Self {
            Self {
                items,
                item_count_override: None,
                representation_count_override: None,
                clock,
                representation_count_calls: Rc::new(Cell::new(0)),
                type_calls: Rc::new(Cell::new(0)),
                data_calls: Rc::new(Cell::new(0)),
                copy_calls: Rc::new(Cell::new(0)),
            }
        }
    }

    impl SnapshotReader for FakeSnapshotReader {
        type TypeName = FakeTypeName;
        type Data = FakeData;

        fn item_count(&mut self) -> Result<usize, InsertError> {
            Ok(self.item_count_override.unwrap_or(self.items.len()))
        }

        fn representation_count(&mut self, item: usize) -> Result<usize, InsertError> {
            self.representation_count_calls
                .set(self.representation_count_calls.get() + 1);
            Ok(self
                .representation_count_override
                .unwrap_or(self.items[item].len()))
        }

        fn type_name(
            &mut self,
            item: usize,
            representation: usize,
        ) -> Result<Self::TypeName, InsertError> {
            self.type_calls.set(self.type_calls.get() + 1);
            let value = &self.items[item][representation];
            Ok(FakeTypeName {
                item,
                representation,
                value: value.type_name,
                utf16_units: value.type_units,
            })
        }

        fn type_name_utf16_units(&self, type_name: &Self::TypeName) -> usize {
            type_name.utf16_units
        }

        fn type_name_to_string(
            &mut self,
            type_name: &Self::TypeName,
        ) -> Result<String, InsertError> {
            Ok(type_name.value.to_owned())
        }

        fn representation_data(
            &mut self,
            type_name: &Self::TypeName,
        ) -> Result<Self::Data, InsertError> {
            self.data_calls.set(self.data_calls.get() + 1);
            let value = &self.items[type_name.item][type_name.representation];
            self.clock.advance(value.data_delay);
            Ok(FakeData {
                bytes: value.bytes.clone().ok_or(InsertError::PasteboardSnapshot)?,
                reported_len: value.reported_len,
                copy_calls: Rc::clone(&self.copy_calls),
            })
        }

        fn data_len(&self, data: &Self::Data) -> usize {
            data.reported_len
        }

        fn copy_data(&mut self, data: Self::Data) -> Result<Vec<u8>, InsertError> {
            data.copy_calls.set(data.copy_calls.get() + 1);
            Ok(data.bytes)
        }
    }

    struct ReaderPasteboard {
        reader: FakeSnapshotReader,
        clock: ManualSnapshotClock,
        temporary_write_counter: Rc<Cell<usize>>,
    }

    struct LateFinalOwnershipPasteboard {
        clock: ManualSnapshotClock,
        temporary_write_counter: Rc<Cell<usize>>,
        restore_counter: Rc<Cell<usize>>,
    }

    impl PasteboardAccess for LateFinalOwnershipPasteboard {
        fn change_count(&self) -> isize {
            0
        }

        fn snapshot(&mut self) -> Result<SnapshotCapture, InsertError> {
            let budget = SnapshotBudget::new(SnapshotLimits::default());
            self.clock.advance(Duration::from_millis(490));
            let snapshot = PasteboardSnapshot::default();
            let expected_change_count = 0;
            self.clock.advance(Duration::from_millis(20));
            let observed_change_count = 0;
            finish_snapshot_capture(
                snapshot,
                expected_change_count,
                observed_change_count,
                &budget,
                &self.clock,
            )
        }

        fn write_temporary_text(
            &mut self,
            _text: &str,
            _expected_change_count: isize,
        ) -> Result<TemporaryWrite, TemporaryWriteFailure> {
            self.temporary_write_counter
                .set(self.temporary_write_counter.get() + 1);
            Ok(TemporaryWrite { change_count: 1 })
        }

        fn restore(
            &mut self,
            _snapshot: &PasteboardSnapshot,
            _expected_change_count: isize,
        ) -> Result<(), InsertError> {
            self.restore_counter.set(self.restore_counter.get() + 1);
            Ok(())
        }
    }

    impl PasteboardAccess for ReaderPasteboard {
        fn change_count(&self) -> isize {
            0
        }

        fn snapshot(&mut self) -> Result<SnapshotCapture, InsertError> {
            capture_snapshot(&mut self.reader, SnapshotLimits::default(), &self.clock).map(
                |snapshot| SnapshotCapture {
                    snapshot,
                    change_count: 0,
                },
            )
        }

        fn write_temporary_text(
            &mut self,
            _text: &str,
            _expected_change_count: isize,
        ) -> Result<TemporaryWrite, TemporaryWriteFailure> {
            self.temporary_write_counter
                .set(self.temporary_write_counter.get() + 1);
            Ok(TemporaryWrite { change_count: 1 })
        }

        fn restore(
            &mut self,
            _snapshot: &PasteboardSnapshot,
            _expected_change_count: isize,
        ) -> Result<(), InsertError> {
            Ok(())
        }
    }

    #[test]
    fn snapshot_budget_accepts_exact_limits_and_rejects_limit_plus_one() {
        let limits = SnapshotLimits::default();
        let mut budget = SnapshotBudget::new(limits);

        assert_eq!(budget.admit_item_count(64), Ok(()));
        assert_eq!(
            budget.admit_item_count(65),
            Err(InsertError::PasteboardSnapshot)
        );
        for _ in 0..8 {
            assert_eq!(budget.admit_representations(32), Ok(()));
        }
        assert_eq!(
            budget.admit_representations(1),
            Err(InsertError::PasteboardSnapshot)
        );
        assert_eq!(budget.admit_type_units(1_024), Ok(()));
        assert_eq!(
            budget.admit_type_units(1_025),
            Err(InsertError::PasteboardSnapshot)
        );

        let mut payload_budget = SnapshotBudget::new(limits);
        assert_eq!(payload_budget.admit_payload(64 * 1024 * 1024), Ok(()));
        assert_eq!(payload_budget.admit_payload(64 * 1024 * 1024), Ok(()));
        assert_eq!(
            payload_budget.admit_payload(1),
            Err(InsertError::PasteboardSnapshot)
        );
        let mut oversized = SnapshotBudget::new(limits);
        assert_eq!(
            oversized.admit_payload(64 * 1024 * 1024 + 1),
            Err(InsertError::PasteboardSnapshot)
        );
    }

    #[test]
    fn snapshot_budget_rejects_checked_counter_overflow() {
        let unlimited = SnapshotLimits {
            max_items: usize::MAX,
            max_representations_per_item: usize::MAX,
            max_representations: usize::MAX,
            max_type_utf16_units: usize::MAX,
            max_representation_bytes: usize::MAX,
            max_payload_bytes: usize::MAX,
            deadline: Duration::from_millis(500),
        };
        let mut representation_budget = SnapshotBudget::new(unlimited);
        representation_budget.representations = usize::MAX;
        assert_eq!(
            representation_budget.admit_representations(1),
            Err(InsertError::PasteboardSnapshot)
        );
        let mut payload_budget = SnapshotBudget::new(unlimited);
        payload_budget.payload_bytes = usize::MAX;
        assert_eq!(
            payload_budget.admit_payload(1),
            Err(InsertError::PasteboardSnapshot)
        );
    }

    #[test]
    fn item_limit_is_checked_before_representation_enumeration() {
        let clock = ManualSnapshotClock::new();
        let mut reader = FakeSnapshotReader::new(Vec::new(), clock.clone());
        reader.item_count_override = Some(65);
        let count_calls = Rc::clone(&reader.representation_count_calls);

        assert_eq!(
            capture_snapshot(&mut reader, SnapshotLimits::default(), &clock),
            Err(InsertError::PasteboardSnapshot)
        );
        assert_eq!(count_calls.get(), 0);
    }

    #[test]
    fn per_item_representation_limit_is_checked_before_type_fetch() {
        let clock = ManualSnapshotClock::new();
        let mut reader = FakeSnapshotReader::new(vec![Vec::new()], clock.clone());
        reader.representation_count_override = Some(33);
        let type_calls = Rc::clone(&reader.type_calls);

        assert_eq!(
            capture_snapshot(&mut reader, SnapshotLimits::default(), &clock),
            Err(InsertError::PasteboardSnapshot)
        );
        assert_eq!(type_calls.get(), 0);
    }

    #[test]
    fn aggregate_representation_limit_counts_zero_byte_payloads() {
        let clock = ManualSnapshotClock::new();
        let mut reader = FakeSnapshotReader::new(
            vec![
                vec![FakeRepresentation::bytes("type.one", b"")],
                vec![FakeRepresentation::bytes("type.two", b"")],
            ],
            clock.clone(),
        );
        let type_calls = Rc::clone(&reader.type_calls);
        let limits = SnapshotLimits {
            max_representations: 1,
            ..SnapshotLimits::default()
        };

        assert_eq!(
            capture_snapshot(&mut reader, limits, &clock),
            Err(InsertError::PasteboardSnapshot)
        );
        assert_eq!(type_calls.get(), 1);
    }

    #[test]
    fn type_name_limit_is_checked_before_data_fetch() {
        let clock = ManualSnapshotClock::new();
        let mut representation = FakeRepresentation::bytes("type", b"data");
        representation.type_units = 1_025;
        let mut reader = FakeSnapshotReader::new(vec![vec![representation]], clock.clone());
        let data_calls = Rc::clone(&reader.data_calls);

        assert_eq!(
            capture_snapshot(&mut reader, SnapshotLimits::default(), &clock),
            Err(InsertError::PasteboardSnapshot)
        );
        assert_eq!(data_calls.get(), 0);
    }

    #[test]
    fn representation_byte_limit_is_checked_before_byte_access() {
        let clock = ManualSnapshotClock::new();
        let mut representation = FakeRepresentation::bytes("type", b"small");
        representation.reported_len = 64 * 1024 * 1024 + 1;
        let mut reader = FakeSnapshotReader::new(vec![vec![representation]], clock.clone());
        let copy_calls = Rc::clone(&reader.copy_calls);

        assert_eq!(
            capture_snapshot(&mut reader, SnapshotLimits::default(), &clock),
            Err(InsertError::PasteboardSnapshot)
        );
        assert_eq!(copy_calls.get(), 0);
    }

    #[test]
    fn aggregate_payload_limit_refuses_before_copying_the_overflowing_value() {
        let clock = ManualSnapshotClock::new();
        let mut reader = FakeSnapshotReader::new(
            vec![vec![
                FakeRepresentation::bytes("type.one", b"12345"),
                FakeRepresentation::bytes("type.two", b"67890"),
            ]],
            clock.clone(),
        );
        let copy_calls = Rc::clone(&reader.copy_calls);
        let limits = SnapshotLimits {
            max_representation_bytes: 8,
            max_payload_bytes: 8,
            ..SnapshotLimits::default()
        };

        assert_eq!(
            capture_snapshot(&mut reader, limits, &clock),
            Err(InsertError::PasteboardSnapshot)
        );
        assert_eq!(copy_calls.get(), 1);
    }

    #[test]
    fn complete_snapshot_preserves_every_item_representation_and_byte() {
        let clock = ManualSnapshotClock::new();
        let mut reader = FakeSnapshotReader::new(
            vec![
                vec![
                    FakeRepresentation::bytes("public.text", b"plain"),
                    FakeRepresentation::bytes("public.rtf", b"{\\rtf1 rich}"),
                ],
                vec![FakeRepresentation::bytes(
                    "public.png",
                    &[0x89, 0x50, 0x4e, 0x47],
                )],
            ],
            clock.clone(),
        );

        assert_eq!(
            capture_snapshot(&mut reader, SnapshotLimits::default(), &clock),
            Ok(snapshot(&[
                &[("public.text", b"plain"), ("public.rtf", b"{\\rtf1 rich}"),],
                &[("public.png", &[0x89, 0x50, 0x4e, 0x47])],
            ]))
        );
    }

    #[test]
    fn missing_representation_after_partial_read_prevents_transaction_write() {
        let clock = ManualSnapshotClock::new();
        let mut missing = FakeRepresentation::bytes("type.two", b"");
        missing.bytes = None;
        let reader = FakeSnapshotReader::new(
            vec![vec![FakeRepresentation::bytes("type.one", b"one"), missing]],
            clock.clone(),
        );
        let temporary_write_counter = Rc::new(Cell::new(0));

        assert_eq!(
            InsertionTransaction::begin_with(
                "recognized",
                ReaderPasteboard {
                    reader,
                    clock,
                    temporary_write_counter: Rc::clone(&temporary_write_counter),
                },
                FakePasteCommand::succeed(),
            )
            .map(|_| ()),
            Err(InsertError::PasteboardSnapshot)
        );
        assert_eq!(temporary_write_counter.get(), 0);
    }

    #[test]
    fn cooperative_snapshot_deadline_after_native_return_prevents_write() {
        let clock = ManualSnapshotClock::new();
        let mut delayed = FakeRepresentation::bytes("type.one", b"one");
        delayed.data_delay = Duration::from_millis(501);
        let reader = FakeSnapshotReader::new(vec![vec![delayed]], clock.clone());
        let copy_calls = Rc::clone(&reader.copy_calls);
        let temporary_write_counter = Rc::new(Cell::new(0));

        assert_eq!(
            InsertionTransaction::begin_with(
                "recognized",
                ReaderPasteboard {
                    reader,
                    clock,
                    temporary_write_counter: Rc::clone(&temporary_write_counter),
                },
                FakePasteCommand::succeed(),
            )
            .map(|_| ()),
            Err(InsertError::PasteboardSnapshot)
        );
        assert_eq!(copy_calls.get(), 0);
        assert_eq!(temporary_write_counter.get(), 0);
    }

    #[test]
    fn late_matching_final_ownership_query_prevents_write_and_rollback() {
        let temporary_write_counter = Rc::new(Cell::new(0));
        let restore_counter = Rc::new(Cell::new(0));

        assert_eq!(
            InsertionTransaction::begin_with(
                "recognized",
                LateFinalOwnershipPasteboard {
                    clock: ManualSnapshotClock::new(),
                    temporary_write_counter: Rc::clone(&temporary_write_counter),
                    restore_counter: Rc::clone(&restore_counter),
                },
                FakePasteCommand::succeed(),
            )
            .map(|_| ()),
            Err(InsertError::PasteboardSnapshot)
        );
        assert_eq!(temporary_write_counter.get(), 0);
        assert_eq!(restore_counter.get(), 0);
    }

    #[test]
    fn nil_pasteboard_items_is_an_error_instead_of_an_empty_snapshot() {
        assert_eq!(
            pasteboard_items_or_error::<usize>(None),
            Err(InsertError::PasteboardSnapshot)
        );
    }

    #[test]
    fn copy_during_settle_cancels_paste_and_preserves_new_contents() {
        let original = snapshot(&[&[("public.utf8-plain-text", b"before")]]);
        let newer = snapshot(&[&[("public.utf8-plain-text", b"user copy")]]);
        let mut insertion = InsertionTransaction::begin_with(
            "recognized",
            FakePasteboard::with_snapshot(original),
            FakePasteCommand::succeed(),
        )
        .unwrap();

        insertion.pasteboard.current = newer.clone();
        insertion.pasteboard.change_count += 1;
        let error = insertion.paste().unwrap_err();

        assert_eq!(error, InsertError::PasteboardChanged);
        assert_eq!(insertion.command.calls, 0);
        assert_eq!(insertion.restore_after_paste_failure(error), error);
        assert_eq!(insertion.pasteboard.current, newer);
        assert_eq!(insertion.pasteboard.restore_calls, 0);
    }

    #[test]
    fn copying_the_same_text_still_transfers_pasteboard_ownership() {
        let mut insertion = InsertionTransaction::begin_with(
            "recognized",
            FakePasteboard::with_snapshot(PasteboardSnapshot::default()),
            FakePasteCommand::succeed(),
        )
        .unwrap();
        let copied = insertion.pasteboard.current.clone();
        insertion.pasteboard.change_count += 1;

        assert_eq!(insertion.paste(), Err(InsertError::PasteboardChanged));
        insertion.restore().unwrap();
        assert_eq!(insertion.command.calls, 0);
        assert_eq!(insertion.pasteboard.current, copied);
    }

    #[test]
    fn insertion_runs_as_explicit_non_blocking_stages() {
        let original = snapshot(&[&[("public.utf8-plain-text", b"before")]]);
        let pasteboard = FakePasteboard::with_snapshot(original.clone());
        let command = FakePasteCommand::succeed();

        let mut insertion =
            InsertionTransaction::begin_with("recognized", pasteboard, command).unwrap();
        assert_eq!(
            insertion.pasteboard.current,
            snapshot(&[&[("public.utf8-plain-text", b"recognized")]])
        );

        assert_eq!(insertion.paste(), Ok(()));
        assert_eq!(
            insertion.pasteboard.current,
            snapshot(&[&[("public.utf8-plain-text", b"recognized")]])
        );

        assert_eq!(insertion.restore(), Ok(()));
        assert_eq!(insertion.pasteboard.current, original);
    }

    #[test]
    fn failed_paste_restores_immediately_and_keeps_primary_error() {
        let original = snapshot(&[&[("public.utf8-plain-text", b"before")]]);
        let pasteboard = FakePasteboard::with_snapshot(original.clone());
        let command = FakePasteCommand::fail(InsertError::KeyboardEvent);
        let mut insertion =
            InsertionTransaction::begin_with("recognized", pasteboard, command).unwrap();

        let primary = insertion.paste().unwrap_err();

        assert_eq!(
            insertion.restore_after_paste_failure(primary),
            InsertError::KeyboardEvent
        );
        assert_eq!(insertion.pasteboard.current, original);
    }

    #[test]
    fn restores_original_snapshot_after_paste() {
        let original = snapshot(&[
            &[("public.utf8-plain-text", b"before")],
            &[("public.png", &[0x89, 0x50, 0x4e, 0x47])],
        ]);
        let mut insertion = InsertionTransaction::begin_with(
            "recognized",
            FakePasteboard::with_snapshot(original.clone()),
            FakePasteCommand::succeed(),
        )
        .unwrap();

        assert_eq!(insertion.paste(), Ok(()));
        assert_eq!(insertion.restore(), Ok(()));
        assert_eq!(insertion.pasteboard.current, original);
    }

    #[test]
    fn does_not_overwrite_a_newer_pasteboard_change() {
        let original = snapshot(&[&[("public.utf8-plain-text", b"before")]]);
        let newer = snapshot(&[&[("public.utf8-plain-text", b"newer")]]);
        let mut pasteboard = FakePasteboard::with_snapshot(original);
        pasteboard.replace_before_ownership_check = Some(newer.clone());
        let mut insertion =
            InsertionTransaction::begin_with("recognized", pasteboard, FakePasteCommand::succeed())
                .unwrap();

        assert_eq!(insertion.paste(), Ok(()));
        assert_eq!(insertion.restore(), Ok(()));
        assert_eq!(insertion.pasteboard.current, newer);
        assert_eq!(insertion.pasteboard.restore_calls, 0);
    }

    #[test]
    fn does_not_overwrite_a_change_during_restore_preparation() {
        let original = snapshot(&[&[("public.utf8-plain-text", b"before")]]);
        let newer = snapshot(&[&[("public.utf8-plain-text", b"newer")]]);
        let mut pasteboard = FakePasteboard::with_snapshot(original);
        pasteboard.replace_during_restore = Some(newer.clone());
        let mut insertion =
            InsertionTransaction::begin_with("recognized", pasteboard, FakePasteCommand::succeed())
                .unwrap();

        assert_eq!(insertion.paste(), Ok(()));
        assert_eq!(insertion.restore(), Ok(()));
        assert_eq!(insertion.pasteboard.current, newer);
    }

    #[test]
    fn reports_restore_failure_after_successful_paste() {
        let mut pasteboard = FakePasteboard::with_snapshot(PasteboardSnapshot::default());
        pasteboard.restore_error = Some(InsertError::PasteboardRestore);
        let mut insertion =
            InsertionTransaction::begin_with("recognized", pasteboard, FakePasteCommand::succeed())
                .unwrap();

        assert_eq!(insertion.paste(), Ok(()));
        assert_eq!(insertion.restore(), Err(InsertError::PasteboardRestore));
    }

    #[test]
    fn dropping_an_active_transaction_restores_the_original_pasteboard() {
        let pasteboard =
            FakePasteboard::with_snapshot(snapshot(&[&[("public.utf8-plain-text", b"before")]]));
        let restore_counter = Rc::clone(&pasteboard.restore_counter);

        drop(
            InsertionTransaction::begin_with("recognized", pasteboard, FakePasteCommand::succeed())
                .unwrap(),
        );

        assert_eq!(restore_counter.get(), 1);
    }

    #[test]
    fn explicit_restore_then_drop_does_not_restore_twice() {
        let pasteboard =
            FakePasteboard::with_snapshot(snapshot(&[&[("public.utf8-plain-text", b"before")]]));
        let restore_counter = Rc::clone(&pasteboard.restore_counter);
        let mut insertion =
            InsertionTransaction::begin_with("recognized", pasteboard, FakePasteCommand::succeed())
                .unwrap();

        insertion.restore().unwrap();
        drop(insertion);

        assert_eq!(restore_counter.get(), 1);
    }

    #[test]
    fn restores_after_keyboard_failure_and_keeps_primary_error() {
        let original = snapshot(&[&[("public.utf8-plain-text", b"before")]]);
        let mut pasteboard = FakePasteboard::with_snapshot(original);
        pasteboard.restore_error = Some(InsertError::PasteboardRestore);
        let mut insertion = InsertionTransaction::begin_with(
            "recognized",
            pasteboard,
            FakePasteCommand::fail(InsertError::KeyboardEvent),
        )
        .unwrap();

        let primary = insertion.paste().unwrap_err();
        assert_eq!(
            insertion.restore_after_paste_failure(primary),
            InsertError::KeyboardEvent
        );
        assert_eq!(insertion.pasteboard.restore_calls, 1);
    }

    #[test]
    fn restores_after_temporary_write_failure_and_keeps_primary_error() {
        let original = snapshot(&[&[("public.utf8-plain-text", b"before")]]);
        let mut pasteboard = FakePasteboard::with_snapshot(original);
        pasteboard.temporary_write_error = Some(InsertError::PasteboardWrite);
        let restore_counter = Rc::clone(&pasteboard.restore_counter);

        assert_eq!(
            InsertionTransaction::begin_with(
                "recognized",
                pasteboard,
                FakePasteCommand::succeed(),
            )
            .map(|_| ()),
            Err(InsertError::PasteboardWrite)
        );
        assert_eq!(restore_counter.get(), 1);
    }

    #[test]
    fn ownership_change_during_snapshot_causes_no_write_or_rollback() {
        let original = snapshot(&[&[("public.utf8-plain-text", b"before")]]);
        let newer = snapshot(&[&[("public.utf8-plain-text", b"user copy")]]);
        let mut pasteboard = FakePasteboard::with_snapshot(original);
        pasteboard.replace_during_snapshot = Some(newer);
        let temporary_write_counter = Rc::clone(&pasteboard.temporary_write_counter);
        let restore_counter = Rc::clone(&pasteboard.restore_counter);

        assert_eq!(
            InsertionTransaction::begin_with(
                "recognized",
                pasteboard,
                FakePasteCommand::succeed(),
            )
            .map(|_| ()),
            Err(InsertError::PasteboardChanged)
        );
        assert_eq!(temporary_write_counter.get(), 0);
        assert_eq!(restore_counter.get(), 0);
    }

    #[test]
    fn ownership_change_immediately_before_clear_causes_no_write_or_rollback() {
        let original = snapshot(&[&[("public.utf8-plain-text", b"before")]]);
        let newer = snapshot(&[&[("public.utf8-plain-text", b"user copy")]]);
        let mut pasteboard = FakePasteboard::with_snapshot(original);
        pasteboard.replace_before_temporary_write = Some(newer);
        let temporary_write_counter = Rc::clone(&pasteboard.temporary_write_counter);
        let restore_counter = Rc::clone(&pasteboard.restore_counter);

        assert_eq!(
            InsertionTransaction::begin_with(
                "recognized",
                pasteboard,
                FakePasteCommand::succeed(),
            )
            .map(|_| ()),
            Err(InsertError::PasteboardChanged)
        );
        assert_eq!(temporary_write_counter.get(), 0);
        assert_eq!(restore_counter.get(), 0);
    }

    #[test]
    fn snapshot_failure_aborts_before_temporary_write() {
        let mut pasteboard = FakePasteboard::with_snapshot(PasteboardSnapshot::default());
        pasteboard.snapshot_error = Some(InsertError::PasteboardSnapshot);
        let temporary_write_counter = Rc::clone(&pasteboard.temporary_write_counter);

        assert_eq!(
            InsertionTransaction::begin_with(
                "recognized",
                pasteboard,
                FakePasteCommand::succeed(),
            )
            .map(|_| ()),
            Err(InsertError::PasteboardSnapshot)
        );
        assert_eq!(temporary_write_counter.get(), 0);
    }

    #[test]
    fn empty_prepared_text_aborts_before_snapshot() {
        let pasteboard = FakePasteboard::with_snapshot(PasteboardSnapshot::default());
        let temporary_write_counter = Rc::clone(&pasteboard.temporary_write_counter);

        assert_eq!(
            InsertionTransaction::begin_with("", pasteboard, FakePasteCommand::succeed())
                .map(|_| ()),
            Err(InsertError::EmptyText)
        );
        assert_eq!(temporary_write_counter.get(), 0);
    }

    #[test]
    fn trims_outer_whitespace_only() {
        assert_eq!(
            normalize_text("  Привет. \n", false),
            Some("Привет.".into())
        );
    }

    #[test]
    fn rejects_whitespace_only_text() {
        assert_eq!(normalize_text(" \n\t ", true), None);
    }

    #[test]
    fn preserves_text_without_rewriting() {
        assert_eq!(
            normalize_text("Текст без точки", false),
            Some("Текст без точки".into())
        );
    }

    #[test]
    fn appends_one_space_without_rewriting_model_punctuation() {
        assert_eq!(
            normalize_text("  Привет. \n", true),
            Some("Привет. ".into())
        );
        assert_eq!(
            normalize_text("Текст без точки", true),
            Some("Текст без точки ".into())
        );
        assert_eq!(normalize_text("Готово!", true), Some("Готово! ".into()));
        assert_eq!(normalize_text("Готово?", true), Some("Готово? ".into()));
    }
}
