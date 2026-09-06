use std::ffi::c_void;
use std::ptr::null;
use std::time::{Duration, Instant};

use core_foundation::base::{CFType, CFTypeID, CFTypeRef, TCFType};
use core_foundation::string::{CFString, CFStringRef};

use crate::inserter::{normalize_text, InsertError, PendingInsertion};

type AXUIElementRef = *const c_void;
type AXError = i32;

const AX_SUCCESS: AXError = 0;
const AX_ERROR_ATTRIBUTE_UNSUPPORTED: AXError = -25205;
const AX_ERROR_NO_VALUE: AXError = -25212;
const AX_COPY_TIMEOUT: Duration = Duration::from_millis(200);
const AX_PROBE_TIMEOUT: Duration = Duration::from_millis(500);
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
        let clock = SystemProbeClock::new();
        ensure_not_secure_with(&mut SystemAxAccess, &clock)
    }
}

trait ProbeClock {
    fn elapsed(&self) -> Duration;
}

struct SystemProbeClock {
    started: Instant,
}

impl SystemProbeClock {
    fn new() -> Self {
        Self {
            started: Instant::now(),
        }
    }
}

impl ProbeClock for SystemProbeClock {
    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }
}

struct AxProbeBudget {
    deadline: Duration,
}

impl AxProbeBudget {
    fn new<C: ProbeClock>(clock: &C) -> Self {
        Self {
            deadline: clock.elapsed().saturating_add(AX_PROBE_TIMEOUT),
        }
    }

    fn remaining<C: ProbeClock>(
        &self,
        clock: &C,
        attribute: &'static str,
    ) -> Result<Duration, InsertError> {
        let elapsed = clock.elapsed();
        let Some(remaining) = self.deadline.checked_sub(elapsed) else {
            return Err(ax_probe_timeout(elapsed, attribute));
        };
        if remaining.is_zero() {
            return Err(ax_probe_timeout(elapsed, attribute));
        }
        Ok(remaining)
    }

    fn check<C: ProbeClock>(&self, clock: &C, attribute: &'static str) -> Result<(), InsertError> {
        self.remaining(clock, attribute).map(|_| ())
    }
}

fn ax_probe_timeout(elapsed: Duration, attribute: &'static str) -> InsertError {
    tracing::warn!(
        error_category = "accessibility_probe_timeout",
        elapsed_ms = elapsed.as_millis() as u64,
        limit_ms = AX_PROBE_TIMEOUT.as_millis() as u64,
        ax_attribute = attribute,
    );
    InsertError::accessibility("probe_timeout", Some(attribute), None)
}

trait AxAccess {
    type Element;
    type Value;

    fn create_system_wide(&mut self) -> Result<Self::Element, InsertError>;
    fn set_messaging_timeout(
        &mut self,
        element: &Self::Element,
        attribute: &'static str,
        timeout: Duration,
    ) -> Result<(), InsertError>;
    fn copy_attribute(
        &mut self,
        element: &Self::Element,
        attribute: &'static str,
        requirement: AxAttributeRequirement,
    ) -> Result<Option<Self::Value>, InsertError>;
    fn value_into_element(&mut self, value: Self::Value) -> Result<Self::Element, InsertError>;
    fn value_into_string(
        &mut self,
        value: Self::Value,
        attribute: &'static str,
    ) -> Result<String, InsertError>;
}

struct SystemAxAccess;

impl AxAccess for SystemAxAccess {
    type Element = CFType;
    type Value = CFType;

    fn create_system_wide(&mut self) -> Result<Self::Element, InsertError> {
        let system = unsafe { AXUIElementCreateSystemWide() };
        if system.is_null() {
            return Err(InsertError::accessibility("create_system_wide", None, None));
        }
        Ok(unsafe { CFType::wrap_under_create_rule(system.cast()) })
    }

    fn set_messaging_timeout(
        &mut self,
        element: &Self::Element,
        attribute: &'static str,
        timeout: Duration,
    ) -> Result<(), InsertError> {
        let status = unsafe {
            AXUIElementSetMessagingTimeout(element.as_CFTypeRef().cast(), timeout.as_secs_f32())
        };
        if status == AX_SUCCESS {
            Ok(())
        } else {
            Err(InsertError::accessibility(
                "set_messaging_timeout",
                Some(attribute),
                Some(status),
            ))
        }
    }

    fn copy_attribute(
        &mut self,
        element: &Self::Element,
        attribute: &'static str,
        requirement: AxAttributeRequirement,
    ) -> Result<Option<Self::Value>, InsertError> {
        copy_ax_attribute(element.as_CFTypeRef().cast(), attribute, requirement)
    }

    fn value_into_element(&mut self, value: Self::Value) -> Result<Self::Element, InsertError> {
        if value.type_of() == unsafe { AXUIElementGetTypeID() } {
            Ok(value)
        } else {
            Err(InsertError::accessibility(
                "decode_attribute",
                Some(AX_FOCUSED_UI_ELEMENT_ATTRIBUTE),
                None,
            ))
        }
    }

    fn value_into_string(
        &mut self,
        value: Self::Value,
        attribute: &'static str,
    ) -> Result<String, InsertError> {
        value
            .downcast_into::<CFString>()
            .map(|value| value.to_string())
            .ok_or_else(|| InsertError::accessibility("decode_attribute", Some(attribute), None))
    }
}

fn copy_bounded_ax_attribute<A: AxAccess, C: ProbeClock>(
    access: &mut A,
    clock: &C,
    budget: &AxProbeBudget,
    element: &A::Element,
    attribute: &'static str,
    requirement: AxAttributeRequirement,
) -> Result<Option<A::Value>, InsertError> {
    let timeout = budget.remaining(clock, attribute)?.min(AX_COPY_TIMEOUT);
    access.set_messaging_timeout(element, attribute, timeout)?;
    budget.check(clock, attribute)?;
    let value = access.copy_attribute(element, attribute, requirement)?;
    budget.check(clock, attribute)?;
    Ok(value)
}

fn ensure_not_secure_with<A: AxAccess, C: ProbeClock>(
    access: &mut A,
    clock: &C,
) -> Result<(), InsertError> {
    let budget = AxProbeBudget::new(clock);
    budget.check(clock, AX_FOCUSED_UI_ELEMENT_ATTRIBUTE)?;
    let system = access.create_system_wide()?;
    budget.check(clock, AX_FOCUSED_UI_ELEMENT_ATTRIBUTE)?;
    let focused = copy_bounded_ax_attribute(
        access,
        clock,
        &budget,
        &system,
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
    let focused = access.value_into_element(focused)?;
    budget.check(clock, AX_FOCUSED_UI_ELEMENT_ATTRIBUTE)?;

    let role = copy_bounded_ax_attribute(
        access,
        clock,
        &budget,
        &focused,
        AX_ROLE_ATTRIBUTE,
        AxAttributeRequirement::Required,
    )?
    .ok_or_else(|| {
        InsertError::accessibility("missing_attribute", Some(AX_ROLE_ATTRIBUTE), None)
    })?;
    let role = access.value_into_string(role, AX_ROLE_ATTRIBUTE)?;
    budget.check(clock, AX_ROLE_ATTRIBUTE)?;

    let subrole = copy_bounded_ax_attribute(
        access,
        clock,
        &budget,
        &focused,
        AX_SUBROLE_ATTRIBUTE,
        AxAttributeRequirement::Optional,
    )?
    .map(|value| access.value_into_string(value, AX_SUBROLE_ATTRIBUTE))
    .transpose()?;
    budget.check(clock, AX_SUBROLE_ATTRIBUTE)?;

    if is_secure_ax_field(&role, subrole.as_deref()) {
        return Err(InsertError::SecureField);
    }
    Ok(())
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
    fn AXUIElementGetTypeID() -> CFTypeID;
    fn AXUIElementSetMessagingTimeout(element: AXUIElementRef, timeout_in_seconds: f32) -> AXError;
    fn AXUIElementCopyAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: *mut CFTypeRef,
    ) -> AXError;
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;
    use std::time::Duration;

    use super::{
        begin_with, classify_ax_attribute, ensure_not_secure_with, is_secure_ax_field, paste_with,
        AccessibilityProbe, AxAccess, AxAttributeRequirement, ClipboardInsertion, ClipboardPaste,
        ProbeClock, AX_COPY_TIMEOUT, AX_ERROR_ATTRIBUTE_UNSUPPORTED, AX_ERROR_NO_VALUE,
        AX_FOCUSED_UI_ELEMENT_ATTRIBUTE, AX_PROBE_TIMEOUT, AX_ROLE_ATTRIBUTE,
        AX_SECURE_TEXT_FIELD_SUBROLE, AX_SUBROLE_ATTRIBUTE, AX_SUCCESS,
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
    fn missing_required_focus_keeps_attribute_and_no_value_error() {
        let error = classify_ax_attribute(
            AX_ERROR_NO_VALUE,
            true,
            AX_FOCUSED_UI_ELEMENT_ATTRIBUTE,
            AxAttributeRequirement::Required,
        )
        .unwrap_err();

        assert_eq!(error.diagnostic_stage(), Some("copy_attribute"));
        assert_eq!(error.ax_attribute(), Some(AX_FOCUSED_UI_ELEMENT_ATTRIBUTE));
        assert_eq!(error.ax_error_code(), Some(AX_ERROR_NO_VALUE));
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

    #[derive(Debug, Clone, Copy, Eq, PartialEq)]
    enum FakeElement {
        System,
        Focused,
    }

    #[derive(Debug, Clone, Eq, PartialEq)]
    enum FakeValue {
        Element(FakeElement),
        String(&'static str),
        InvalidElement,
    }

    #[derive(Debug, Clone, Eq, PartialEq)]
    enum AxCall {
        CreateSystemWide,
        SetTimeout(FakeElement, &'static str, Duration),
        Copy(FakeElement, &'static str),
        DecodeElement,
        DecodeString(&'static str),
    }

    #[derive(Clone)]
    struct ManualClock {
        elapsed: Rc<RefCell<Duration>>,
    }

    impl ManualClock {
        fn new() -> Self {
            Self {
                elapsed: Rc::new(RefCell::new(Duration::ZERO)),
            }
        }

        fn advance(&self, duration: Duration) {
            *self.elapsed.borrow_mut() += duration;
        }
    }

    impl ProbeClock for ManualClock {
        fn elapsed(&self) -> Duration {
            *self.elapsed.borrow()
        }
    }

    struct FakeAxAccess {
        calls: Vec<AxCall>,
        clock: ManualClock,
        copy_advances: VecDeque<Duration>,
        decode_advances: VecDeque<Duration>,
        setter_failure: Option<(&'static str, i32)>,
        focused_value: FakeValue,
        role: &'static str,
        subrole: Option<&'static str>,
    }

    impl FakeAxAccess {
        fn normal(clock: ManualClock) -> Self {
            Self {
                calls: Vec::new(),
                clock,
                copy_advances: VecDeque::new(),
                decode_advances: VecDeque::new(),
                setter_failure: None,
                focused_value: FakeValue::Element(FakeElement::Focused),
                role: "AXTextArea",
                subrole: None,
            }
        }
    }

    impl AxAccess for FakeAxAccess {
        type Element = FakeElement;
        type Value = FakeValue;

        fn create_system_wide(&mut self) -> Result<Self::Element, InsertError> {
            self.calls.push(AxCall::CreateSystemWide);
            Ok(FakeElement::System)
        }

        fn set_messaging_timeout(
            &mut self,
            element: &Self::Element,
            attribute: &'static str,
            timeout: Duration,
        ) -> Result<(), InsertError> {
            self.calls
                .push(AxCall::SetTimeout(*element, attribute, timeout));
            if let Some((failed_attribute, status)) = self.setter_failure {
                if failed_attribute == attribute {
                    return Err(InsertError::accessibility(
                        "set_messaging_timeout",
                        Some(attribute),
                        Some(status),
                    ));
                }
            }
            Ok(())
        }

        fn copy_attribute(
            &mut self,
            element: &Self::Element,
            attribute: &'static str,
            _requirement: AxAttributeRequirement,
        ) -> Result<Option<Self::Value>, InsertError> {
            self.calls.push(AxCall::Copy(*element, attribute));
            self.clock
                .advance(self.copy_advances.pop_front().unwrap_or_default());
            match attribute {
                AX_FOCUSED_UI_ELEMENT_ATTRIBUTE => Ok(Some(self.focused_value.clone())),
                AX_ROLE_ATTRIBUTE => Ok(Some(FakeValue::String(self.role))),
                AX_SUBROLE_ATTRIBUTE => Ok(self.subrole.map(FakeValue::String)),
                _ => unreachable!(),
            }
        }

        fn value_into_element(&mut self, value: Self::Value) -> Result<Self::Element, InsertError> {
            self.calls.push(AxCall::DecodeElement);
            self.clock
                .advance(self.decode_advances.pop_front().unwrap_or_default());
            match value {
                FakeValue::Element(element) => Ok(element),
                FakeValue::String(_) | FakeValue::InvalidElement => {
                    Err(InsertError::accessibility(
                        "decode_attribute",
                        Some(AX_FOCUSED_UI_ELEMENT_ATTRIBUTE),
                        None,
                    ))
                }
            }
        }

        fn value_into_string(
            &mut self,
            value: Self::Value,
            attribute: &'static str,
        ) -> Result<String, InsertError> {
            self.calls.push(AxCall::DecodeString(attribute));
            self.clock
                .advance(self.decode_advances.pop_front().unwrap_or_default());
            match value {
                FakeValue::String(value) => Ok(value.to_owned()),
                FakeValue::Element(_) | FakeValue::InvalidElement => Err(
                    InsertError::accessibility("decode_attribute", Some(attribute), None),
                ),
            }
        }
    }

    #[test]
    fn ax_probe_sets_positive_bounded_timeout_before_every_copy() {
        let clock = ManualClock::new();
        let mut access = FakeAxAccess::normal(clock.clone());
        access.copy_advances = VecDeque::from([
            Duration::from_millis(250),
            Duration::from_millis(150),
            Duration::ZERO,
        ]);

        assert_eq!(ensure_not_secure_with(&mut access, &clock), Ok(()));

        let timeouts = access
            .calls
            .iter()
            .filter_map(|call| match call {
                AxCall::SetTimeout(_, attribute, timeout) => Some((*attribute, *timeout)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            timeouts,
            [
                (AX_FOCUSED_UI_ELEMENT_ATTRIBUTE, AX_COPY_TIMEOUT),
                (AX_ROLE_ATTRIBUTE, AX_COPY_TIMEOUT),
                (AX_SUBROLE_ATTRIBUTE, Duration::from_millis(100)),
            ]
        );
        assert!(timeouts.iter().all(|(_, timeout)| !timeout.is_zero()));
    }

    #[test]
    fn ax_timeout_setter_failure_prevents_attribute_copy() {
        let clock = ManualClock::new();
        let mut access = FakeAxAccess::normal(clock.clone());
        access.setter_failure = Some((AX_ROLE_ATTRIBUTE, -25204));

        let error = ensure_not_secure_with(&mut access, &clock).unwrap_err();

        assert_eq!(error.diagnostic_stage(), Some("set_messaging_timeout"));
        assert_eq!(error.ax_attribute(), Some(AX_ROLE_ATTRIBUTE));
        assert_eq!(error.ax_error_code(), Some(-25204));
        assert!(!access
            .calls
            .contains(&AxCall::Copy(FakeElement::Focused, AX_ROLE_ATTRIBUTE)));
    }

    #[test]
    fn expired_ax_probe_stops_before_the_next_native_call() {
        let clock = ManualClock::new();
        let mut access = FakeAxAccess::normal(clock.clone());
        access.copy_advances = VecDeque::from([AX_PROBE_TIMEOUT]);

        let error = ensure_not_secure_with(&mut access, &clock).unwrap_err();

        assert_eq!(error.diagnostic_stage(), Some("probe_timeout"));
        assert!(!access.calls.iter().any(|call| matches!(
            call,
            AxCall::SetTimeout(FakeElement::Focused, _, _) | AxCall::Copy(FakeElement::Focused, _)
        )));
    }

    #[test]
    fn late_final_ax_decode_cannot_authorize_insertion() {
        let clock = ManualClock::new();
        let mut access = FakeAxAccess::normal(clock.clone());
        access.subrole = Some("AXStandardWindow");
        access.decode_advances = VecDeque::from([
            Duration::ZERO,
            Duration::ZERO,
            AX_PROBE_TIMEOUT + Duration::from_millis(1),
        ]);

        let error = ensure_not_secure_with(&mut access, &clock).unwrap_err();

        assert_eq!(error.diagnostic_stage(), Some("probe_timeout"));
        assert_eq!(
            access.calls.last(),
            Some(&AxCall::DecodeString(AX_SUBROLE_ATTRIBUTE))
        );
    }

    #[test]
    fn invalid_focused_value_is_rejected_before_focused_ax_calls() {
        let clock = ManualClock::new();
        let mut access = FakeAxAccess::normal(clock.clone());
        access.focused_value = FakeValue::InvalidElement;

        let error = ensure_not_secure_with(&mut access, &clock).unwrap_err();

        assert_eq!(error.diagnostic_stage(), Some("decode_attribute"));
        assert_eq!(error.ax_attribute(), Some(AX_FOCUSED_UI_ELEMENT_ATTRIBUTE));
        assert!(!access.calls.iter().any(|call| matches!(
            call,
            AxCall::SetTimeout(FakeElement::Focused, _, _) | AxCall::Copy(FakeElement::Focused, _)
        )));
    }

    #[test]
    fn bounded_ax_probe_still_rejects_protected_fields() {
        let clock = ManualClock::new();
        let mut access = FakeAxAccess::normal(clock.clone());
        access.subrole = Some(AX_SECURE_TEXT_FIELD_SUBROLE);

        assert_eq!(
            ensure_not_secure_with(&mut access, &clock),
            Err(InsertError::SecureField)
        );
    }
}
