use std::ffi::c_void;
use std::ptr::null;

use core_foundation::base::{CFType, CFTypeRef, TCFType};
use core_foundation::string::{CFString, CFStringRef};

use crate::inserter::{normalize_text, InsertError, PendingInsertion};

type AXUIElementRef = *const c_void;
type AXError = i32;

const AX_SUCCESS: AXError = 0;
const AX_ERROR_NO_VALUE: AXError = -25212;
const AX_FOCUSED_UI_ELEMENT_ATTRIBUTE: &str = "AXFocusedUIElement";
const AX_ROLE_ATTRIBUTE: &str = "AXRole";
const AX_SUBROLE_ATTRIBUTE: &str = "AXSubrole";
const AX_SECURE_TEXT_FIELD_SUBROLE: &str = "AXSecureTextField";

pub(crate) trait AccessibilityProbe {
    fn ensure_not_secure(&mut self) -> Result<(), InsertError>;
}

pub(crate) trait ClipboardInsertion {
    type Pending;

    fn begin_clipboard(&mut self, text: &str) -> Result<Self::Pending, InsertError>;
}

trait ClipboardPaste {
    fn paste(&mut self) -> Result<(), InsertError>;
}

pub(crate) fn begin_with<A, C>(
    text: &str,
    append_space: bool,
    accessibility: &mut A,
    clipboard: &mut C,
) -> Result<C::Pending, InsertError>
where
    A: AccessibilityProbe,
    C: ClipboardInsertion,
{
    let text = normalize_text(text, append_space).ok_or(InsertError::EmptyText)?;
    accessibility.ensure_not_secure()?;
    clipboard.begin_clipboard(&text)
}

struct SystemAccessibility;

impl AccessibilityProbe for SystemAccessibility {
    fn ensure_not_secure(&mut self) -> Result<(), InsertError> {
        let system = unsafe { AXUIElementCreateSystemWide() };
        if system.is_null() {
            return Err(InsertError::Accessibility);
        }
        let system = unsafe { CFType::wrap_under_create_rule(system.cast()) };
        let focused_attribute = CFString::from_static_string(AX_FOCUSED_UI_ELEMENT_ATTRIBUTE);

        let focused = copy_ax_attribute(
            system.as_CFTypeRef().cast(),
            focused_attribute.as_concrete_TypeRef(),
        )?
        .ok_or(InsertError::Accessibility)?;
        let focused_ref = focused.as_CFTypeRef().cast();

        let role_attribute = CFString::from_static_string(AX_ROLE_ATTRIBUTE);
        let role = copy_ax_attribute(focused_ref, role_attribute.as_concrete_TypeRef())?
            .ok_or(InsertError::Accessibility)?
            .downcast_into::<CFString>()
            .ok_or(InsertError::Accessibility)?;

        let subrole_attribute = CFString::from_static_string(AX_SUBROLE_ATTRIBUTE);
        let subrole = match copy_ax_attribute(focused_ref, subrole_attribute.as_concrete_TypeRef())?
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
        Ok(())
    }
}

fn is_secure_ax_field(role: &str, subrole: Option<&str>) -> bool {
    role == AX_SECURE_TEXT_FIELD_SUBROLE || subrole == Some(AX_SECURE_TEXT_FIELD_SUBROLE)
}

fn copy_ax_attribute(
    element: AXUIElementRef,
    attribute: CFStringRef,
) -> Result<Option<CFType>, InsertError> {
    let mut value: CFTypeRef = null();
    let status = unsafe { AXUIElementCopyAttributeValue(element, attribute, &mut value) };
    match ax_attribute_has_value(status, value.is_null())? {
        true => Ok(Some(unsafe { CFType::wrap_under_create_rule(value) })),
        false => Ok(None),
    }
}

fn ax_attribute_has_value(status: AXError, value_is_null: bool) -> Result<bool, InsertError> {
    match (status, value_is_null) {
        (AX_SUCCESS, false) => Ok(true),
        (AX_ERROR_NO_VALUE, true) => Ok(false),
        _ => Err(InsertError::Accessibility),
    }
}

struct SystemClipboard;

impl ClipboardInsertion for SystemClipboard {
    type Pending = PendingInsertion;

    fn begin_clipboard(&mut self, text: &str) -> Result<Self::Pending, InsertError> {
        PendingInsertion::begin(text)
    }
}

impl ClipboardPaste for PendingInsertion {
    fn paste(&mut self) -> Result<(), InsertError> {
        PendingInsertion::paste(self)
    }
}

fn paste_with<A, P>(accessibility: &mut A, insertion: &mut P) -> Result<(), InsertError>
where
    A: AccessibilityProbe,
    P: ClipboardPaste,
{
    accessibility.ensure_not_secure()?;
    insertion.paste()
}

pub(crate) struct PendingTextInsertion {
    inner: PendingInsertion,
}

impl PendingTextInsertion {
    pub(crate) fn paste(&mut self) -> Result<(), InsertError> {
        paste_with(&mut SystemAccessibility, &mut self.inner)
    }

    pub(crate) fn restore(&mut self) -> Result<(), InsertError> {
        self.inner.restore()
    }

    pub(crate) fn restore_after_paste_failure(&mut self, primary: InsertError) -> InsertError {
        self.inner.restore_after_paste_failure(primary)
    }
}

pub(crate) fn begin(text: &str, append_space: bool) -> Result<PendingTextInsertion, InsertError> {
    let inner = begin_with(
        text,
        append_space,
        &mut SystemAccessibility,
        &mut SystemClipboard,
    )?;
    Ok(PendingTextInsertion { inner })
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXUIElementCreateSystemWide() -> AXUIElementRef;
    fn AXUIElementCopyAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: *mut CFTypeRef,
    ) -> AXError;
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::{
        ax_attribute_has_value, begin_with, is_secure_ax_field, paste_with, AccessibilityProbe,
        ClipboardInsertion, ClipboardPaste, AX_ERROR_NO_VALUE, AX_SUCCESS,
    };
    use crate::inserter::InsertError;

    struct RecordingAx {
        calls: Rc<RefCell<Vec<&'static str>>>,
        result: Result<(), InsertError>,
    }

    impl AccessibilityProbe for RecordingAx {
        fn ensure_not_secure(&mut self) -> Result<(), InsertError> {
            self.calls.borrow_mut().push("ax");
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

    struct RecordingPending {
        calls: Rc<RefCell<Vec<&'static str>>>,
    }

    impl ClipboardPaste for RecordingPending {
        fn paste(&mut self) -> Result<(), InsertError> {
            self.calls.borrow_mut().push("paste");
            Ok(())
        }
    }

    type Boundaries = (
        RecordingAx,
        RecordingClipboard,
        Rc<RefCell<Vec<&'static str>>>,
        Rc<RefCell<Vec<String>>>,
    );

    fn boundaries(ax_result: Result<(), InsertError>) -> Boundaries {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let texts = Rc::new(RefCell::new(Vec::new()));
        (
            RecordingAx {
                calls: Rc::clone(&calls),
                result: ax_result,
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
    fn accessibility_success_still_uses_clipboard_for_compatible_input_events() {
        let (mut ax, mut clipboard, calls, texts) = boundaries(Ok(()));

        let outcome = begin_with(" Привет. ", true, &mut ax, &mut clipboard);

        assert_eq!(outcome, Ok("pending"));
        assert_eq!(calls.borrow().as_slice(), ["ax", "clipboard"]);
        assert_eq!(texts.borrow().as_slice(), ["Привет. "]);
    }

    #[test]
    fn disabled_preference_preserves_punctuation_without_trailing_space() {
        let (mut ax, mut clipboard, calls, texts) = boundaries(Ok(()));

        let outcome = begin_with(" Привет. ", false, &mut ax, &mut clipboard);

        assert_eq!(outcome, Ok("pending"));
        assert_eq!(calls.borrow().as_slice(), ["ax", "clipboard"]);
        assert_eq!(texts.borrow().as_slice(), ["Привет."]);
    }

    #[test]
    fn secure_field_error_is_terminal() {
        let (mut ax, mut clipboard, calls, _texts) = boundaries(Err(InsertError::SecureField));

        let outcome = begin_with("текст", false, &mut ax, &mut clipboard);

        assert_eq!(outcome, Err(InsertError::SecureField));
        assert_eq!(calls.borrow().as_slice(), ["ax"]);
    }

    #[test]
    fn accessibility_probe_error_is_terminal() {
        let (mut ax, mut clipboard, calls, _texts) = boundaries(Err(InsertError::Accessibility));

        let outcome = begin_with("текст", false, &mut ax, &mut clipboard);

        assert_eq!(outcome, Err(InsertError::Accessibility));
        assert_eq!(calls.borrow().as_slice(), ["ax"]);
    }

    #[test]
    fn paste_rechecks_accessibility_immediately_before_command_v() {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let mut ax = RecordingAx {
            calls: Rc::clone(&calls),
            result: Ok(()),
        };
        let mut pending = RecordingPending {
            calls: Rc::clone(&calls),
        };

        assert_eq!(paste_with(&mut ax, &mut pending), Ok(()));
        assert_eq!(calls.borrow().as_slice(), ["ax", "paste"]);
    }

    #[test]
    fn secure_focus_at_paste_time_blocks_command_v() {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let mut ax = RecordingAx {
            calls: Rc::clone(&calls),
            result: Err(InsertError::SecureField),
        };
        let mut pending = RecordingPending {
            calls: Rc::clone(&calls),
        };

        assert_eq!(
            paste_with(&mut ax, &mut pending),
            Err(InsertError::SecureField)
        );
        assert_eq!(calls.borrow().as_slice(), ["ax"]);
    }

    #[test]
    fn ax_attribute_errors_are_not_treated_as_missing_values() {
        assert_eq!(ax_attribute_has_value(AX_SUCCESS, false), Ok(true));
        assert_eq!(ax_attribute_has_value(AX_ERROR_NO_VALUE, true), Ok(false));
        assert_eq!(
            ax_attribute_has_value(-25204, true),
            Err(InsertError::Accessibility)
        );
        assert_eq!(
            ax_attribute_has_value(AX_SUCCESS, true),
            Err(InsertError::Accessibility)
        );
    }

    #[test]
    fn whitespace_only_text_is_rejected_before_any_adapter() {
        let (mut ax, mut clipboard, calls, texts) = boundaries(Ok(()));

        let outcome = begin_with(" \n\t ", true, &mut ax, &mut clipboard);

        assert_eq!(outcome, Err(InsertError::EmptyText));
        assert!(calls.borrow().is_empty());
        assert!(texts.borrow().is_empty());
    }

    #[test]
    fn secure_text_field_is_detected_from_ax_subrole() {
        assert!(is_secure_ax_field("AXTextField", Some("AXSecureTextField")));
        assert!(!is_secure_ax_field("AXTextField", None));
        assert!(!is_secure_ax_field("AXTextArea", Some("AXStandardWindow")));
    }
}
