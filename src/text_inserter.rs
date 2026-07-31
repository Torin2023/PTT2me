use std::ffi::c_void;
use std::ptr::null;

use core_foundation::base::{CFType, CFTypeRef, TCFType};
use core_foundation::string::{CFString, CFStringRef};
use core_graphics::event::{CGEvent, CGEventTapLocation};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

use crate::inserter::{normalize_text, InsertError, PendingInsertion};

type AXUIElementRef = *const c_void;
type AXError = i32;

const AX_SUCCESS: AXError = 0;
const AX_FOCUSED_UI_ELEMENT_ATTRIBUTE: &str = "AXFocusedUIElement";
const AX_ROLE_ATTRIBUTE: &str = "AXRole";
const AX_SUBROLE_ATTRIBUTE: &str = "AXSubrole";
const AX_SELECTED_TEXT_ATTRIBUTE: &str = "AXSelectedText";
const AX_SECURE_TEXT_FIELD_SUBROLE: &str = "AXSecureTextField";
const UNICODE_CHUNK_CHARS: usize = 32;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum InsertMethod {
    Accessibility,
    UnicodeEvents,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum InsertOutcome<P> {
    Complete(InsertMethod),
    PendingClipboard(P),
}

pub(crate) trait AccessibilityInsertion {
    fn insert_selected_text(&mut self, text: &str) -> Result<bool, InsertError>;
}

pub(crate) trait UnicodeInsertion {
    fn insert_unicode(&mut self, text: &str) -> Result<bool, InsertError>;
}

pub(crate) trait ClipboardInsertion {
    type Pending;

    fn begin_clipboard(&mut self, text: &str) -> Result<Self::Pending, InsertError>;
}

pub(crate) fn begin_with<A, U, C>(
    text: &str,
    append_space: bool,
    accessibility: &mut A,
    unicode: &mut U,
    clipboard: &mut C,
) -> Result<InsertOutcome<C::Pending>, InsertError>
where
    A: AccessibilityInsertion,
    U: UnicodeInsertion,
    C: ClipboardInsertion,
{
    let text = normalize_text(text, append_space).ok_or(InsertError::EmptyText)?;
    match accessibility.insert_selected_text(&text) {
        Ok(true) => Ok(InsertOutcome::Complete(InsertMethod::Accessibility)),
        Err(error) => Err(error),
        Ok(false) => match unicode.insert_unicode(&text)? {
            true => Ok(InsertOutcome::Complete(InsertMethod::UnicodeEvents)),
            false => clipboard
                .begin_clipboard(&text)
                .map(InsertOutcome::PendingClipboard),
        },
    }
}

struct SystemAccessibility;

impl AccessibilityInsertion for SystemAccessibility {
    fn insert_selected_text(&mut self, text: &str) -> Result<bool, InsertError> {
        let system = unsafe { AXUIElementCreateSystemWide() };
        if system.is_null() {
            return Err(InsertError::Accessibility);
        }
        let system = unsafe { CFType::wrap_under_create_rule(system.cast()) };
        let focused_attribute = CFString::from_static_string(AX_FOCUSED_UI_ELEMENT_ATTRIBUTE);

        let Some(focused) = copy_ax_attribute(
            system.as_CFTypeRef().cast(),
            focused_attribute.as_concrete_TypeRef(),
        ) else {
            return Ok(false);
        };
        let focused_ref = focused.as_CFTypeRef().cast();

        let role_attribute = CFString::from_static_string(AX_ROLE_ATTRIBUTE);
        let role = copy_ax_attribute(focused_ref, role_attribute.as_concrete_TypeRef())
            .ok_or(InsertError::Accessibility)?
            .downcast_into::<CFString>()
            .ok_or(InsertError::Accessibility)?;

        let subrole_attribute = CFString::from_static_string(AX_SUBROLE_ATTRIBUTE);
        let subrole = match copy_ax_attribute(focused_ref, subrole_attribute.as_concrete_TypeRef())
        {
            Some(value) => Some(
                value
                    .downcast_into::<CFString>()
                    .ok_or(InsertError::Accessibility)?,
            ),
            None => None,
        };
        let role = role.to_string();
        let subrole = subrole.map(|value| value.to_string());
        if is_secure_ax_field(&role, subrole.as_deref()) {
            return Err(InsertError::SecureField);
        }

        let mut settable = 0_u8;
        let selected_text_attribute = CFString::from_static_string(AX_SELECTED_TEXT_ATTRIBUTE);
        let settable_status = unsafe {
            AXUIElementIsAttributeSettable(
                focused_ref,
                selected_text_attribute.as_concrete_TypeRef(),
                &mut settable,
            )
        };
        if settable_status != AX_SUCCESS || settable == 0 {
            return Ok(false);
        }

        let value = CFString::new(text);
        let status = unsafe {
            AXUIElementSetAttributeValue(
                focused_ref,
                selected_text_attribute.as_concrete_TypeRef(),
                value.as_CFTypeRef(),
            )
        };
        if status == AX_SUCCESS {
            Ok(true)
        } else {
            Err(InsertError::Accessibility)
        }
    }
}

fn is_secure_ax_field(role: &str, subrole: Option<&str>) -> bool {
    role == AX_SECURE_TEXT_FIELD_SUBROLE || subrole == Some(AX_SECURE_TEXT_FIELD_SUBROLE)
}

fn copy_ax_attribute(element: AXUIElementRef, attribute: CFStringRef) -> Option<CFType> {
    let mut value: CFTypeRef = null();
    let status = unsafe { AXUIElementCopyAttributeValue(element, attribute, &mut value) };
    if status != AX_SUCCESS || value.is_null() {
        None
    } else {
        Some(unsafe { CFType::wrap_under_create_rule(value) })
    }
}

struct SystemUnicode;

impl UnicodeInsertion for SystemUnicode {
    fn insert_unicode(&mut self, text: &str) -> Result<bool, InsertError> {
        let Ok(source) = CGEventSource::new(CGEventSourceStateID::Private) else {
            return Ok(false);
        };
        let mut events = Vec::new();
        let mut chunk = String::new();

        for character in text.chars() {
            chunk.push(character);
            if chunk.chars().count() == UNICODE_CHUNK_CHARS {
                let Some(pair) = unicode_event_pair(&source, &chunk) else {
                    return Ok(false);
                };
                events.push(pair);
                chunk.clear();
            }
        }
        if !chunk.is_empty() {
            let Some(pair) = unicode_event_pair(&source, &chunk) else {
                return Ok(false);
            };
            events.push(pair);
        }

        for (key_down, key_up) in events {
            key_down.post(CGEventTapLocation::HID);
            key_up.post(CGEventTapLocation::HID);
        }
        Ok(true)
    }
}

fn unicode_event_pair(source: &CGEventSource, text: &str) -> Option<(CGEvent, CGEvent)> {
    let key_down = CGEvent::new_keyboard_event(source.clone(), 0, true).ok()?;
    let key_up = CGEvent::new_keyboard_event(source.clone(), 0, false).ok()?;
    key_down.set_string(text);
    Some((key_down, key_up))
}

struct SystemClipboard;

impl ClipboardInsertion for SystemClipboard {
    type Pending = PendingInsertion;

    fn begin_clipboard(&mut self, text: &str) -> Result<Self::Pending, InsertError> {
        PendingInsertion::begin(text)
    }
}

pub(crate) fn begin(
    text: &str,
    append_space: bool,
) -> Result<InsertOutcome<PendingInsertion>, InsertError> {
    begin_with(
        text,
        append_space,
        &mut SystemAccessibility,
        &mut SystemUnicode,
        &mut SystemClipboard,
    )
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXUIElementCreateSystemWide() -> AXUIElementRef;
    fn AXUIElementCopyAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: *mut CFTypeRef,
    ) -> AXError;
    fn AXUIElementIsAttributeSettable(
        element: AXUIElementRef,
        attribute: CFStringRef,
        settable: *mut u8,
    ) -> AXError;
    fn AXUIElementSetAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: CFTypeRef,
    ) -> AXError;
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::{
        begin_with, is_secure_ax_field, AccessibilityInsertion, ClipboardInsertion, InsertMethod,
        InsertOutcome, UnicodeInsertion,
    };
    use crate::inserter::InsertError;

    struct RecordingAx {
        calls: Rc<RefCell<Vec<&'static str>>>,
        texts: Rc<RefCell<Vec<String>>>,
        result: Result<bool, InsertError>,
    }

    impl AccessibilityInsertion for RecordingAx {
        fn insert_selected_text(&mut self, text: &str) -> Result<bool, InsertError> {
            self.calls.borrow_mut().push("ax");
            self.texts.borrow_mut().push(text.to_owned());
            self.result
        }
    }

    struct RecordingUnicode {
        calls: Rc<RefCell<Vec<&'static str>>>,
        texts: Rc<RefCell<Vec<String>>>,
        result: Result<bool, InsertError>,
    }

    impl UnicodeInsertion for RecordingUnicode {
        fn insert_unicode(&mut self, text: &str) -> Result<bool, InsertError> {
            self.calls.borrow_mut().push("unicode");
            self.texts.borrow_mut().push(text.to_owned());
            self.result
        }
    }

    struct RecordingClipboard {
        calls: Rc<RefCell<Vec<&'static str>>>,
        texts: Rc<RefCell<Vec<String>>>,
    }

    impl ClipboardInsertion for RecordingClipboard {
        type Pending = &'static str;

        fn begin_clipboard(&mut self, text: &str) -> Result<Self::Pending, InsertError> {
            self.calls.borrow_mut().push("clipboard");
            self.texts.borrow_mut().push(text.to_owned());
            Ok("pending")
        }
    }

    type Boundaries = (
        RecordingAx,
        RecordingUnicode,
        RecordingClipboard,
        Rc<RefCell<Vec<&'static str>>>,
        Rc<RefCell<Vec<String>>>,
    );

    fn boundaries(
        ax_result: Result<bool, InsertError>,
        unicode_result: Result<bool, InsertError>,
    ) -> Boundaries {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let texts = Rc::new(RefCell::new(Vec::new()));
        (
            RecordingAx {
                calls: Rc::clone(&calls),
                texts: Rc::clone(&texts),
                result: ax_result,
            },
            RecordingUnicode {
                calls: Rc::clone(&calls),
                texts: Rc::clone(&texts),
                result: unicode_result,
            },
            RecordingClipboard {
                calls: Rc::clone(&calls),
                texts: Rc::clone(&texts),
            },
            calls,
            texts,
        )
    }

    #[test]
    fn accessibility_success_bypasses_unicode_and_clipboard() {
        let (mut ax, mut unicode, mut clipboard, calls, texts) = boundaries(Ok(true), Ok(true));

        let outcome = begin_with(" Привет. ", true, &mut ax, &mut unicode, &mut clipboard);

        assert_eq!(
            outcome,
            Ok(InsertOutcome::Complete(InsertMethod::Accessibility))
        );
        assert_eq!(calls.borrow().as_slice(), ["ax"]);
        assert_eq!(texts.borrow().as_slice(), ["Привет. "]);
    }

    #[test]
    fn unsupported_accessibility_falls_through_to_unicode() {
        let (mut ax, mut unicode, mut clipboard, calls, texts) = boundaries(Ok(false), Ok(true));

        let outcome = begin_with(" Привет. ", true, &mut ax, &mut unicode, &mut clipboard);

        assert_eq!(
            outcome,
            Ok(InsertOutcome::Complete(InsertMethod::UnicodeEvents))
        );
        assert_eq!(calls.borrow().as_slice(), ["ax", "unicode"]);
        assert_eq!(texts.borrow().as_slice(), ["Привет. ", "Привет. "]);
    }

    #[test]
    fn unsupported_unicode_falls_through_to_clipboard() {
        let (mut ax, mut unicode, mut clipboard, calls, texts) = boundaries(Ok(false), Ok(false));

        let outcome = begin_with(" Привет. ", true, &mut ax, &mut unicode, &mut clipboard);

        assert_eq!(outcome, Ok(InsertOutcome::PendingClipboard("pending")));
        assert_eq!(calls.borrow().as_slice(), ["ax", "unicode", "clipboard"]);
        assert_eq!(
            texts.borrow().as_slice(),
            ["Привет. ", "Привет. ", "Привет. "]
        );
    }

    #[test]
    fn secure_field_error_is_terminal() {
        let (mut ax, mut unicode, mut clipboard, calls, _texts) =
            boundaries(Err(InsertError::SecureField), Ok(true));

        let outcome = begin_with("текст", false, &mut ax, &mut unicode, &mut clipboard);

        assert_eq!(outcome, Err(InsertError::SecureField));
        assert_eq!(calls.borrow().as_slice(), ["ax"]);
    }

    #[test]
    fn secure_text_field_is_detected_from_ax_subrole() {
        assert!(is_secure_ax_field("AXTextField", Some("AXSecureTextField")));
        assert!(!is_secure_ax_field("AXTextField", None));
        assert!(!is_secure_ax_field("AXTextArea", Some("AXStandardWindow")));
    }
}
