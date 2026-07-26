use std::{error::Error, fmt, thread, time::Duration};

use core_graphics::{
    event::{CGEvent, CGEventFlags, CGEventTapLocation},
    event_source::{CGEventSource, CGEventSourceStateID},
};
use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
use objc2_foundation::NSString;

const PASTE_KEYCODE: u16 = 9;
const PASTEBOARD_SETTLE_DELAY: Duration = Duration::from_millis(30);

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum InsertError {
    EmptyText,
    PasteboardWrite,
    EventSource,
    KeyboardEvent,
}

impl fmt::Display for InsertError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::EmptyText => "cannot insert empty text",
            Self::PasteboardWrite => "could not write to the pasteboard",
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
    use super::normalize_text;

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
