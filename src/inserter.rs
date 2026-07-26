use std::{error::Error, fmt, thread, time::Duration};

use core_graphics::{
    event::{CGEvent, CGEventFlags, CGEventTapLocation},
    event_source::{CGEventSource, CGEventSourceStateID},
};
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_app_kit::{NSPasteboard, NSPasteboardItem, NSPasteboardTypeString, NSPasteboardWriting};
use objc2_foundation::{NSArray, NSData, NSString};

const PASTE_KEYCODE: u16 = 9;
const PASTEBOARD_SETTLE_DELAY: Duration = Duration::from_millis(30);
const PASTEBOARD_RESTORE_DELAY: Duration = Duration::from_millis(100);

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

trait Sleeper {
    fn sleep(&mut self, duration: Duration);
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum InsertError {
    EmptyText,
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
            Self::PasteboardSnapshot => "could not snapshot the pasteboard",
            Self::PasteboardWrite => "could not write to the pasteboard",
            Self::PasteboardRestore => "could not restore the pasteboard",
            Self::EventSource => "could not create a keyboard event source",
            Self::KeyboardEvent => "could not create a paste keyboard event",
        })
    }
}

impl Error for InsertError {}

/// Removes outer whitespace without modifying recognised text itself.
pub fn normalize_text(text: &str) -> Option<String> {
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
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

struct ThreadSleeper;

impl Sleeper for ThreadSleeper {
    fn sleep(&mut self, duration: Duration) {
        thread::sleep(duration);
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

fn insert_with(
    text: &str,
    pasteboard: &mut impl PasteboardAccess,
    command: &mut impl PasteCommand,
    sleeper: &mut impl Sleeper,
) -> Result<(), InsertError> {
    let text = normalize_text(text).ok_or(InsertError::EmptyText)?;
    let snapshot = pasteboard.snapshot()?;
    let temporary = match pasteboard.write_temporary_text(&text) {
        Ok(temporary) => temporary,
        Err(failure) => {
            if pasteboard.restore(&snapshot, failure.change_count).is_err() {
                tracing::warn!(error_category = "pasteboard_restore_after_temporary_write_failure");
            }
            return Err(failure.error);
        }
    };

    sleeper.sleep(PASTEBOARD_SETTLE_DELAY);
    let paste_result = command.send_command_v();
    if paste_result.is_ok() {
        sleeper.sleep(PASTEBOARD_RESTORE_DELAY);
    }

    let restore_result = pasteboard.restore(&snapshot, temporary.change_count);

    match (paste_result, restore_result) {
        (Err(primary), Err(_restore)) => {
            tracing::warn!(error_category = "pasteboard_restore_after_insert_failure");
            Err(primary)
        }
        (Err(primary), Ok(())) => Err(primary),
        (Ok(()), restore) => restore,
    }
}

/// Temporarily writes `text`, sends Command-V to the frontmost application,
/// then restores the previous pasteboard contents when they are still current.
pub fn insert_text(text: &str) -> Result<(), InsertError> {
    let pasteboard = unsafe { NSPasteboard::generalPasteboard() };
    insert_with(
        text,
        &mut SystemPasteboard::new(pasteboard),
        &mut SystemPasteCommand,
        &mut ThreadSleeper,
    )
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

#[cfg(test)]
mod tests {
    use super::*;

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

    #[derive(Default)]
    struct FakeSleeper {
        delays: Vec<Duration>,
    }

    impl Sleeper for FakeSleeper {
        fn sleep(&mut self, duration: Duration) {
            self.delays.push(duration);
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

    fn write_snapshot_fixture(pasteboard: &NSPasteboard, saved: &PasteboardSnapshot) {
        let objects = reconstruct_items(saved).unwrap();
        unsafe {
            pasteboard.clearContents();
            assert!(saved.items.is_empty() || pasteboard.writeObjects(&objects));
        }
    }

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

    #[test]
    fn round_trips_multiple_items_and_representations() {
        let pasteboard = unsafe { NSPasteboard::pasteboardWithUniqueName() };
        let expected = snapshot(&[
            &[
                ("public.utf8-plain-text", b"plain"),
                ("public.rtf", b"{\\rtf1 rich}"),
            ],
            &[("public.png", &[0x89, 0x50, 0x4e, 0x47])],
        ]);
        write_snapshot_fixture(&pasteboard, &expected);
        let mut system = SystemPasteboard::new(pasteboard);

        let captured = system.snapshot().unwrap();
        assert_snapshot_contains(&captured, &expected);
        let temporary = system.write_temporary_text("recognized").unwrap();
        system.restore(&captured, temporary.change_count).unwrap();

        assert_eq!(system.snapshot().unwrap(), captured);
    }

    #[test]
    fn round_trips_an_empty_pasteboard() {
        let pasteboard = unsafe { NSPasteboard::pasteboardWithUniqueName() };
        unsafe { pasteboard.clearContents() };
        let mut system = SystemPasteboard::new(pasteboard);

        let captured = system.snapshot().unwrap();
        let temporary = system.write_temporary_text("recognized").unwrap();
        system.restore(&captured, temporary.change_count).unwrap();

        assert_eq!(system.snapshot().unwrap(), PasteboardSnapshot::default());
    }

    #[test]
    fn restores_original_snapshot_after_paste() {
        let original = snapshot(&[
            &[("public.utf8-plain-text", b"before")],
            &[("public.png", &[0x89, 0x50, 0x4e, 0x47])],
        ]);
        let mut pasteboard = FakePasteboard::with_snapshot(original.clone());
        let mut command = FakePasteCommand::succeed();
        let mut sleeper = FakeSleeper::default();

        assert_eq!(
            insert_with("recognized", &mut pasteboard, &mut command, &mut sleeper),
            Ok(())
        );
        assert_eq!(pasteboard.current, original);
        assert_eq!(
            sleeper.delays,
            vec![PASTEBOARD_SETTLE_DELAY, PASTEBOARD_RESTORE_DELAY]
        );
    }

    #[test]
    fn does_not_overwrite_a_newer_pasteboard_change() {
        let original = snapshot(&[&[("public.utf8-plain-text", b"before")]]);
        let newer = snapshot(&[&[("public.utf8-plain-text", b"newer")]]);
        let mut pasteboard = FakePasteboard::with_snapshot(original);
        pasteboard.replace_before_ownership_check = Some(newer.clone());

        assert_eq!(
            insert_with(
                "recognized",
                &mut pasteboard,
                &mut FakePasteCommand::succeed(),
                &mut FakeSleeper::default(),
            ),
            Ok(())
        );
        assert_eq!(pasteboard.current, newer);
        assert_eq!(pasteboard.restore_calls, 0);
    }

    #[test]
    fn does_not_overwrite_a_change_during_restore_preparation() {
        let original = snapshot(&[&[("public.utf8-plain-text", b"before")]]);
        let newer = snapshot(&[&[("public.utf8-plain-text", b"newer")]]);
        let mut pasteboard = FakePasteboard::with_snapshot(original);
        pasteboard.replace_during_restore = Some(newer.clone());

        assert_eq!(
            insert_with(
                "recognized",
                &mut pasteboard,
                &mut FakePasteCommand::succeed(),
                &mut FakeSleeper::default(),
            ),
            Ok(())
        );
        assert_eq!(pasteboard.current, newer);
    }

    #[test]
    fn reports_restore_failure_after_successful_paste() {
        let mut pasteboard = FakePasteboard::with_snapshot(PasteboardSnapshot::default());
        pasteboard.restore_error = Some(InsertError::PasteboardRestore);

        assert_eq!(
            insert_with(
                "recognized",
                &mut pasteboard,
                &mut FakePasteCommand::succeed(),
                &mut FakeSleeper::default(),
            ),
            Err(InsertError::PasteboardRestore)
        );
    }

    #[test]
    fn restores_after_keyboard_failure_and_keeps_primary_error() {
        let original = snapshot(&[&[("public.utf8-plain-text", b"before")]]);
        let mut pasteboard = FakePasteboard::with_snapshot(original);
        pasteboard.restore_error = Some(InsertError::PasteboardRestore);

        assert_eq!(
            insert_with(
                "recognized",
                &mut pasteboard,
                &mut FakePasteCommand::fail(InsertError::KeyboardEvent),
                &mut FakeSleeper::default(),
            ),
            Err(InsertError::KeyboardEvent)
        );
        assert_eq!(pasteboard.restore_calls, 1);
    }

    #[test]
    fn restores_after_temporary_write_failure_and_keeps_primary_error() {
        let original = snapshot(&[&[("public.utf8-plain-text", b"before")]]);
        let mut pasteboard = FakePasteboard::with_snapshot(original);
        pasteboard.temporary_write_error = Some(InsertError::PasteboardWrite);

        assert_eq!(
            insert_with(
                "recognized",
                &mut pasteboard,
                &mut FakePasteCommand::succeed(),
                &mut FakeSleeper::default(),
            ),
            Err(InsertError::PasteboardWrite)
        );
        assert_eq!(pasteboard.restore_calls, 1);
    }

    #[test]
    fn snapshot_failure_aborts_before_temporary_write() {
        let mut pasteboard = FakePasteboard::with_snapshot(PasteboardSnapshot::default());
        pasteboard.snapshot_error = Some(InsertError::PasteboardSnapshot);

        assert_eq!(
            insert_with(
                "recognized",
                &mut pasteboard,
                &mut FakePasteCommand::succeed(),
                &mut FakeSleeper::default(),
            ),
            Err(InsertError::PasteboardSnapshot)
        );
        assert_eq!(pasteboard.temporary_write_calls, 0);
    }

    #[test]
    fn trims_outer_whitespace_only() {
        assert_eq!(normalize_text("  Привет. \n"), Some("Привет.".into()));
    }

    #[test]
    fn rejects_whitespace_only_text() {
        assert_eq!(normalize_text(" \n\t "), None);
    }

    #[test]
    fn preserves_text_without_rewriting() {
        assert_eq!(
            normalize_text("Текст без точки"),
            Some("Текст без точки".into())
        );
    }
}
