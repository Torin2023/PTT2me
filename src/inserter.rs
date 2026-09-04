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

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct TemporaryWrite {
    change_count: isize,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct TemporaryWriteFailure {
    error: InsertError,
    change_count: isize,
}

trait PasteboardAccess {
    fn snapshot(&mut self) -> Result<PasteboardSnapshot, InsertError>;
    fn write_temporary_text(&mut self, text: &str)
        -> Result<TemporaryWrite, TemporaryWriteFailure>;
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
pub enum InsertError {
    EmptyText,
    SecureField,
    Accessibility,
    PasteboardSnapshot,
    PasteboardWrite,
    PasteboardRestore,
    EventSource,
    KeyboardEvent,
}

impl fmt::Display for InsertError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::EmptyText => "cannot insert empty text",
            Self::SecureField => "cannot insert into a secure text field",
            Self::Accessibility => "could not inspect the focused field through Accessibility",
            Self::PasteboardSnapshot => "could not snapshot the pasteboard",
            Self::PasteboardWrite => "could not write to the pasteboard",
            Self::PasteboardRestore => "could not restore the pasteboard",
            Self::EventSource => "could not create a keyboard event source",
            Self::KeyboardEvent => "could not create a paste keyboard event",
        })
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

impl SystemPasteboard {
    fn new(pasteboard: Retained<NSPasteboard>) -> Self {
        Self { pasteboard }
    }
}

impl PasteboardAccess for SystemPasteboard {
    fn snapshot(&mut self) -> Result<PasteboardSnapshot, InsertError> {
        let Some(items) = (unsafe { self.pasteboard.pasteboardItems() }) else {
            return Ok(PasteboardSnapshot::default());
        };

        let mut captured_items = Vec::with_capacity(items.len());
        for item_index in 0..items.len() {
            let item = items
                .get(item_index)
                .ok_or(InsertError::PasteboardSnapshot)?;
            let types = unsafe { item.types() };
            let mut representations = Vec::with_capacity(types.len());
            for type_index in 0..types.len() {
                let pasteboard_type = types
                    .get(type_index)
                    .ok_or(InsertError::PasteboardSnapshot)?;
                let data = unsafe { item.dataForType(pasteboard_type) }
                    .ok_or(InsertError::PasteboardSnapshot)?;
                representations.push(PasteboardRepresentation {
                    type_name: pasteboard_type.to_string(),
                    data: data.bytes().to_vec(),
                });
            }
            captured_items.push(PasteboardItemSnapshot { representations });
        }

        Ok(PasteboardSnapshot {
            items: captured_items,
        })
    }

    fn write_temporary_text(
        &mut self,
        text: &str,
    ) -> Result<TemporaryWrite, TemporaryWriteFailure> {
        unsafe {
            self.pasteboard.clearContents();
            let text = NSString::from_str(text);
            let written = self
                .pasteboard
                .setString_forType(&text, NSPasteboardTypeString);
            let change_count = self.pasteboard.changeCount();
            if written {
                Ok(TemporaryWrite { change_count })
            } else {
                Err(TemporaryWriteFailure {
                    error: InsertError::PasteboardWrite,
                    change_count,
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
        let snapshot = pasteboard.snapshot()?;
        let temporary = match pasteboard.write_temporary_text(text) {
            Ok(temporary) => temporary,
            Err(failure) => {
                if pasteboard.restore(&snapshot, failure.change_count).is_err() {
                    tracing::warn!(
                        error_category = "pasteboard_restore_after_temporary_write_failure"
                    );
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

    let captured = system.snapshot()?;
    assert_snapshot_contains(&captured, &expected);
    let temporary = system
        .write_temporary_text("recognized")
        .map_err(|failure| failure.error)?;
    system.restore(&captured, temporary.change_count)?;
    assert_eq!(system.snapshot()?, captured);

    let empty_pasteboard = unsafe { NSPasteboard::pasteboardWithUniqueName() };
    unsafe { empty_pasteboard.clearContents() };
    let mut empty_system = SystemPasteboard::new(empty_pasteboard);
    let empty_snapshot = empty_system.snapshot()?;
    let temporary = empty_system
        .write_temporary_text("recognized")
        .map_err(|failure| failure.error)?;
    empty_system.restore(&empty_snapshot, temporary.change_count)?;
    assert_eq!(empty_system.snapshot()?, PasteboardSnapshot::default());

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

    struct FakePasteboard {
        current: PasteboardSnapshot,
        change_count: isize,
        snapshot_error: Option<InsertError>,
        temporary_write_error: Option<InsertError>,
        restore_error: Option<InsertError>,
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
        fn snapshot(&mut self) -> Result<PasteboardSnapshot, InsertError> {
            self.snapshot_error
                .map_or_else(|| Ok(self.current.clone()), Err)
        }

        fn write_temporary_text(
            &mut self,
            text: &str,
        ) -> Result<TemporaryWrite, TemporaryWriteFailure> {
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
                    change_count: result.change_count,
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
    }

    impl FakePasteCommand {
        fn succeed() -> Self {
            Self { result: Ok(()) }
        }

        fn fail(error: InsertError) -> Self {
            Self { result: Err(error) }
        }
    }

    impl PasteCommand for FakePasteCommand {
        fn send_command_v(&mut self) -> Result<(), InsertError> {
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
