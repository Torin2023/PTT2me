use std::{error::Error, fmt, thread, time::Duration};

use core_graphics::{
    event::{CGEvent, CGEventFlags, CGEventTapLocation},
    event_source::{CGEventSource, CGEventSourceStateID},
};
use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
use objc2_foundation::NSString;

const PASTE_KEYCODE: u16 = 9;
const PASTEBOARD_SETTLE_DELAY: Duration = Duration::from_millis(30);
#[cfg(test)]
const PASTEBOARD_RESTORE_DELAY: Duration = Duration::from_millis(100);

#[cfg(test)]
#[derive(Debug, Clone, Eq, PartialEq, Default)]
struct PasteboardSnapshot {
    items: Vec<PasteboardItemSnapshot>,
}

#[cfg(test)]
#[derive(Debug, Clone, Eq, PartialEq)]
struct PasteboardItemSnapshot {
    representations: Vec<PasteboardRepresentation>,
}

#[cfg(test)]
#[derive(Debug, Clone, Eq, PartialEq)]
struct PasteboardRepresentation {
    type_name: String,
    data: Vec<u8>,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct TemporaryWrite {
    change_count: isize,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct TemporaryWriteFailure {
    error: InsertError,
    change_count: isize,
}

#[cfg(test)]
trait PasteboardAccess {
    fn snapshot(&mut self) -> Result<PasteboardSnapshot, InsertError>;
    fn write_temporary_text(&mut self, text: &str)
        -> Result<TemporaryWrite, TemporaryWriteFailure>;
    fn change_count(&mut self) -> isize;
    fn restore(&mut self, snapshot: &PasteboardSnapshot) -> Result<(), InsertError>;
}

#[cfg(test)]
trait PasteCommand {
    fn send_command_v(&mut self) -> Result<(), InsertError>;
}

#[cfg(test)]
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

#[cfg(test)]
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
            if pasteboard.change_count() == failure.change_count
                && pasteboard.restore(&snapshot).is_err()
            {
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

    let restore_result = if pasteboard.change_count() == temporary.change_count {
        pasteboard.restore(&snapshot)
    } else {
        Ok(())
    };

    match (paste_result, restore_result) {
        (Err(primary), Err(_restore)) => {
            tracing::warn!(error_category = "pasteboard_restore_after_insert_failure");
            Err(primary)
        }
        (Err(primary), Ok(())) => Err(primary),
        (Ok(()), restore) => restore,
    }
}

/// Replaces the general pasteboard with `text` and sends Command-V to the
/// frontmost application. The newly recognised text deliberately remains in
/// the pasteboard.
pub fn insert_text(text: &str) -> Result<(), InsertError> {
    let text = normalize_text(text).ok_or(InsertError::EmptyText)?;

    unsafe {
        let pasteboard = NSPasteboard::generalPasteboard();
        pasteboard.clearContents();
        let text = NSString::from_str(&text);
        if !pasteboard.setString_forType(&text, NSPasteboardTypeString) {
            return Err(InsertError::PasteboardWrite);
        }
    }

    thread::sleep(PASTEBOARD_SETTLE_DELAY);

    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .map_err(|_| InsertError::EventSource)?;
    post_command_v(source.clone())?;
    post_command_v_up(source)
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

        fn change_count(&mut self) -> isize {
            if let Some(newer) = self.replace_before_ownership_check.take() {
                self.current = newer;
                self.change_count += 1;
            }
            self.change_count
        }

        fn restore(&mut self, saved: &PasteboardSnapshot) -> Result<(), InsertError> {
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
