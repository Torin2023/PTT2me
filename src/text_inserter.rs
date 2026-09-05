use std::ffi::c_void;
use std::ptr::null;

use core_foundation::base::{CFType, CFTypeRef, TCFType};
use core_foundation::string::{CFString, CFStringRef};

use crate::inserter::{normalize_text, InsertError, PendingInsertion};

type AXUIElementRef = *const c_void;
type AXError = i32;

const AX_SUCCESS: AXError = 0;
const AX_ERROR_ATTRIBUTE_UNSUPPORTED: AXError = -25205;
const AX_ERROR_NO_VALUE: AXError = -25212;
const AX_FOCUSED_UI_ELEMENT_ATTRIBUTE: &str = "AXFocusedUIElement";
const AX_ROLE_ATTRIBUTE: &str = "AXRole";
const AX_SUBROLE_ATTRIBUTE: &str = "AXSubrole";
const AX_SECURE_TEXT_FIELD_SUBROLE: &str = "AXSecureTextField";

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum AxAttributeRequirement {
    Required,
    Optional,
}

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
            return Err(InsertError::accessibility("create_system_wide", None, None));
        }
        let system = unsafe { CFType::wrap_under_create_rule(system.cast()) };

        let focused = copy_ax_attribute(
            system.as_CFTypeRef().cast(),
            AX_FOCUSED_UI_ELEMENT_ATTRIBUTE,
            AxAttributeRequirement::Required,
        )?
        .ok_or_else(|| {
            InsertError::accessibility(
                "missing_attribute",
                Some(AX_FOCUSED_UI_ELEMENT_ATTRIBUTE),
                None,
            )
        })?;
        let focused_ref = focused.as_CFTypeRef().cast();

        let role = copy_ax_attribute(
            focused_ref,
            AX_ROLE_ATTRIBUTE,
            AxAttributeRequirement::Required,
        )?
        .ok_or_else(|| {
            InsertError::accessibility("missing_attribute", Some(AX_ROLE_ATTRIBUTE), None)
        })?
        .downcast_into::<CFString>()
        .ok_or_else(|| {
            InsertError::accessibility("decode_attribute", Some(AX_ROLE_ATTRIBUTE), None)
        })?;

        let subrole = match copy_ax_attribute(
            focused_ref,
            AX_SUBROLE_ATTRIBUTE,
            AxAttributeRequirement::Optional,
        )? {
            Some(value) => Some(value.downcast_into::<CFString>().ok_or_else(|| {
                InsertError::accessibility("decode_attribute", Some(AX_SUBROLE_ATTRIBUTE), None)
            })?),
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
    attribute: &'static str,
    requirement: AxAttributeRequirement,
) -> Result<Option<CFType>, InsertError> {
    let attribute_name = CFString::from_static_string(attribute);
    let mut value: CFTypeRef = null();
    let status = unsafe {
        AXUIElementCopyAttributeValue(element, attribute_name.as_concrete_TypeRef(), &mut value)
    };
    match classify_ax_attribute(status, value.is_null(), attribute, requirement)? {
        true => Ok(Some(unsafe { CFType::wrap_under_create_rule(value) })),
        false => {
            tracing::debug!(
                lifecycle = "accessibility_optional_attribute_unavailable",
                diagnostic_stage = "copy_attribute",
                ax_attribute = attribute,
                ax_error_code = status,
            );
            Ok(None)
        }
    }
}

fn classify_ax_attribute(
    status: AXError,
    value_is_null: bool,
    attribute: &'static str,
    requirement: AxAttributeRequirement,
) -> Result<bool, InsertError> {
    match (status, value_is_null, requirement) {
        (AX_SUCCESS, false, _) => Ok(true),
        (
            AX_ERROR_ATTRIBUTE_UNSUPPORTED | AX_ERROR_NO_VALUE,
            true,
            AxAttributeRequirement::Optional,
        ) => Ok(false),
        _ => Err(InsertError::accessibility(
            "copy_attribute",
            Some(attribute),
            Some(status),
        )),
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
        begin_with, classify_ax_attribute, is_secure_ax_field, paste_with, AccessibilityProbe,
        AxAttributeRequirement, ClipboardInsertion, ClipboardPaste, AX_ERROR_ATTRIBUTE_UNSUPPORTED,
        AX_ERROR_NO_VALUE, AX_ROLE_ATTRIBUTE, AX_SECURE_TEXT_FIELD_SUBROLE, AX_SUBROLE_ATTRIBUTE,
        AX_SUCCESS,
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
        let error = InsertError::accessibility("test_probe", None, None);
        let (mut ax, mut clipboard, calls, _texts) = boundaries(Err(error));

        let outcome = begin_with("текст", false, &mut ax, &mut clipboard);

        assert_eq!(outcome, Err(error));
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
        let optional = AxAttributeRequirement::Optional;
        assert_eq!(
            classify_ax_attribute(AX_SUCCESS, false, AX_SUBROLE_ATTRIBUTE, optional),
            Ok(true)
        );
        assert_eq!(
            classify_ax_attribute(AX_ERROR_NO_VALUE, true, AX_SUBROLE_ATTRIBUTE, optional),
            Ok(false)
        );
        assert!(classify_ax_attribute(-25204, true, AX_SUBROLE_ATTRIBUTE, optional).is_err());
        assert!(classify_ax_attribute(AX_SUCCESS, true, AX_SUBROLE_ATTRIBUTE, optional).is_err());
    }

    #[test]
    fn unsupported_optional_ax_attribute_is_treated_as_missing() {
        assert_eq!(
            classify_ax_attribute(
                AX_ERROR_ATTRIBUTE_UNSUPPORTED,
                true,
                AX_SUBROLE_ATTRIBUTE,
                AxAttributeRequirement::Optional,
            ),
            Ok(false)
        );
    }

    #[test]
    fn unsupported_required_ax_attribute_keeps_attribute_and_error_code() {
        let error = classify_ax_attribute(
            AX_ERROR_ATTRIBUTE_UNSUPPORTED,
            true,
            AX_ROLE_ATTRIBUTE,
            AxAttributeRequirement::Required,
        )
        .unwrap_err();

        assert_eq!(error.diagnostic_stage(), Some("copy_attribute"));
        assert_eq!(error.ax_attribute(), Some(AX_ROLE_ATTRIBUTE));
        assert_eq!(error.ax_error_code(), Some(AX_ERROR_ATTRIBUTE_UNSUPPORTED));
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
        assert!(is_secure_ax_field(AX_SECURE_TEXT_FIELD_SUBROLE, None));
        assert!(is_secure_ax_field("AXTextField", Some("AXSecureTextField")));
        assert!(!is_secure_ax_field("AXTextField", None));
        assert!(!is_secure_ax_field("AXTextArea", Some("AXStandardWindow")));
    }
}
