//! Best-effort, read-only preparation of the frontmost application's AX metadata.

use std::ptr::null;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use core_foundation::array::{CFArray, CFArrayRef};
use core_foundation::base::{CFIndex, CFType, CFTypeID, CFTypeRef, TCFType};
use core_foundation::string::{CFString, CFStringRef};
use objc2_app_kit::NSWorkspace;

const MAX_NODES: usize = 128;
const MAX_DEPTH: usize = 12;
const PREPARATION_BUDGET: Duration = Duration::from_millis(500);
const CALL_BUDGET: Duration = Duration::from_millis(50);
static AX_PROBE_BUSY: AtomicBool = AtomicBool::new(false);

/// Start preparation alongside recording. Only the process ID is needed; apps
/// without bundle identifiers use exactly the same path. Results are diagnostic
/// only. text_inserter independently checks fresh system focus and secure status
/// at both begin and paste time; no discovered node is ever an insertion target.
/// Only the worker deadline is returned, for nonblocking runtime contention.
/// The deadline includes thread scheduling and is never a readiness result.
pub(crate) fn prepare_focused_application() -> Option<Instant> {
    let pid = unsafe { NSWorkspace::sharedWorkspace().frontmostApplication() }
        .map(|application| unsafe { objc2::msg_send![&*application, processIdentifier] });
    dispatch_preparation(pid, request_preparation).flatten()
}

fn dispatch_preparation<R>(
    pid: Option<libc::pid_t>,
    request: impl FnOnce(libc::pid_t) -> R,
) -> Option<R> {
    pid.filter(|pid| *pid > 0).map(request)
}

// System-wide AX messaging timeouts are process-global. Both runtime insertion
// and this worker must hold this lease, including across begin-to-paste delay.
// Acquisition never waits; the runtime retries its existing one-entry queue.
pub(crate) struct AxProbeLease<'a>(&'a AtomicBool);

impl<'a> AxProbeLease<'a> {
    pub(crate) fn acquire(preparing: &'a AtomicBool) -> Option<Self> {
        preparing
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Self(preparing))
    }
}

impl Drop for AxProbeLease<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

pub(crate) fn try_acquire_insertion_probe() -> Option<AxProbeLease<'static>> {
    AxProbeLease::acquire(&AX_PROBE_BUSY)
}

fn request_preparation(pid: libc::pid_t) -> Option<Instant> {
    let guard = AxProbeLease::acquire(&AX_PROBE_BUSY)?;
    let deadline = Instant::now() + PREPARATION_BUDGET;
    // AX IPC never holds the audio/menu run loop. Dropping the captured guard
    // clears single-flight state on success, panic, and failed thread creation.
    std::thread::Builder::new()
        .name("accessibility-preparation".into())
        .spawn(move || {
            let _guard = guard;
            let outcome = prepare_application(pid, deadline);
            tracing::debug!(
                lifecycle = "accessibility_preparation_finished",
                outcome = outcome.diagnostic(),
            );
        })
        .ok()?;
    Some(deadline)
}

fn prepare_application(pid: libc::pid_t, deadline: Instant) -> PreparationOutcome {
    let Some(root) = (unsafe { AxElement::from_created(AXUIElementCreateApplication(pid)) }) else {
        return PreparationOutcome::Unavailable;
    };
    let mut tree = SystemTree {
        application: root.clone(),
        pid,
        deadline,
    };
    prepare_tree(&mut tree, root)
}

#[derive(Debug, PartialEq, Eq)]
enum PreparationOutcome {
    AlreadyReady,
    ReadyAfterPriming,
    Unavailable,
    BudgetExhausted,
}

impl PreparationOutcome {
    fn diagnostic(&self) -> &'static str {
        match self {
            Self::AlreadyReady => "already_ready",
            Self::ReadyAfterPriming => "ready_after_priming",
            Self::Unavailable => "unavailable",
            Self::BudgetExhausted => "budget_exhausted",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Role {
    WebArea,
    Other,
}

trait AccessibilityTree {
    type Node: Clone + PartialEq;

    fn focused_element(&mut self) -> Option<Self::Node>;
    fn role(&mut self, node: &Self::Node) -> Option<Role>;
    fn subrole(&mut self, node: &Self::Node) -> Result<(), ()>;
    fn children(&mut self, node: &Self::Node, limit: usize) -> Vec<Self::Node>;
    fn within_deadline(&self) -> bool;
}

fn focus_metadata_ready<T: AccessibilityTree>(tree: &mut T) -> bool {
    if !tree.within_deadline() {
        return false;
    }
    let Some(focused) = tree.focused_element() else {
        return false;
    };
    if !tree.within_deadline() || tree.role(&focused).is_none() || !tree.within_deadline() {
        return false;
    }
    // Match the inserter's metadata requirements: role required, subrole
    // optional only when AX explicitly reports unsupported/no value. A secure
    // field is also ready metadata, so it needs no traversal. This is NOT a
    // security decision or permission to insert.
    tree.subrole(&focused).is_ok() && tree.within_deadline()
}

fn prepare_tree<T: AccessibilityTree>(tree: &mut T, root: T::Node) -> PreparationOutcome {
    if focus_metadata_ready(tree) {
        return PreparationOutcome::AlreadyReady;
    }
    let exhausted = prime_tree(tree, root);
    if focus_metadata_ready(tree) {
        PreparationOutcome::ReadyAfterPriming
    } else if exhausted || !tree.within_deadline() {
        PreparationOutcome::BudgetExhausted
    } else {
        PreparationOutcome::Unavailable
    }
}

// True means a traversal limit was reached. No nodes escape this worker.
fn prime_tree<T: AccessibilityTree>(tree: &mut T, root: T::Node) -> bool {
    let mut pending = Vec::with_capacity(MAX_NODES);
    pending.push((root, 0));
    let mut visited = Vec::with_capacity(MAX_NODES);
    let mut exhausted = false;
    while let Some((node, depth)) = pending.pop() {
        if !tree.within_deadline() || visited.len() == MAX_NODES {
            return true;
        }
        if visited.contains(&node) {
            continue;
        }
        visited.push(node.clone());
        // Never inspect text, values, selections, URLs, or page descendants.
        if tree.role(&node) == Some(Role::WebArea) {
            break;
        }
        if !tree.within_deadline() {
            return true;
        }
        if depth == MAX_DEPTH || visited.len() == MAX_NODES {
            exhausted = true;
            continue;
        }
        let remaining = MAX_NODES - visited.len() - pending.len();
        if remaining > 0 {
            let children = tree.children(&node, remaining);
            for child in children.into_iter().take(remaining).rev() {
                if !visited.contains(&child) && !pending.iter().any(|(node, _)| node == &child) {
                    pending.push((child, depth + 1));
                }
            }
        }
    }
    exhausted
}

/// Only validated AX objects can cross the FFI boundary. CF equality identifies
/// remote elements even if repeated queries return different wrapper pointers.
#[derive(Clone, PartialEq)]
struct AxElement(CFType);

impl AxElement {
    fn from_value(value: CFType) -> Option<Self> {
        (value.type_of() == unsafe { AXUIElementGetTypeID() }).then_some(Self(value))
    }

    unsafe fn from_created(raw: CFTypeRef) -> Option<Self> {
        if raw.is_null() {
            None
        } else {
            Self::from_value(unsafe { CFType::wrap_under_create_rule(raw) })
        }
    }

    fn raw(&self) -> CFTypeRef {
        self.0.as_CFTypeRef()
    }
}

struct SystemTree {
    application: AxElement,
    pid: libc::pid_t,
    deadline: Instant,
}

fn remaining_call_budget(deadline: Instant, now: Instant) -> Option<Duration> {
    deadline
        .checked_duration_since(now)
        .filter(|left| !left.is_zero())
        .map(|left| left.min(CALL_BUDGET))
}

impl SystemTree {
    fn bound_call(&self, node: &AxElement) -> bool {
        let Some(timeout) = remaining_call_budget(self.deadline, Instant::now()) else {
            return false;
        };
        (unsafe { AXUIElementSetMessagingTimeout(node.raw(), timeout.as_secs_f32()) == 0 })
            && self.within_deadline()
    }

    fn read(
        &self,
        node: &AxElement,
        attribute: &'static str,
        optional: bool,
    ) -> Result<Option<CFType>, ()> {
        let attribute = CFString::from_static_string(attribute);
        if !self.bound_call(node) {
            return Err(());
        }
        let mut value: CFTypeRef = null();
        let status = unsafe {
            AXUIElementCopyAttributeValue(node.raw(), attribute.as_concrete_TypeRef(), &mut value)
        };
        let value = (!value.is_null()).then(|| unsafe { CFType::wrap_under_create_rule(value) });
        if !self.within_deadline() {
            return Err(());
        }
        attribute_result(status, value, optional)
    }
}

// Keep the exact required/optional semantics used by text_inserter. An error
// accompanied by a value is still an error, and the owned value is released.
fn attribute_result(
    status: i32,
    value: Option<CFType>,
    optional: bool,
) -> Result<Option<CFType>, ()> {
    match (status, value, optional) {
        (0, Some(value), _) => Ok(Some(value)),
        (-25205 | -25212, None, true) => Ok(None),
        _ => Err(()),
    }
}

fn element_for_pid(value: CFType, expected: libc::pid_t) -> Option<AxElement> {
    let focused = AxElement::from_value(value)?;
    let mut pid = 0;
    // GetPid reads local AX object metadata, not application IPC.
    let matches = unsafe { AXUIElementGetPid(focused.raw(), &mut pid) == 0 } && pid == expected;
    matches.then_some(focused)
}

fn metadata_role(value: CFType) -> Option<Role> {
    let role = value.downcast_into::<CFString>()?;
    // Compare metadata in place instead of allocating an arbitrary-size String
    // supplied by another process. Only the web-root distinction is needed.
    Some(if role == CFString::from_static_string("AXWebArea") {
        Role::WebArea
    } else {
        Role::Other
    })
}

fn metadata_subrole(value: Option<CFType>) -> Result<(), ()> {
    match value {
        None => Ok(()),
        Some(value) => value.downcast_into::<CFString>().map(|_| ()).ok_or(()),
    }
}

impl AccessibilityTree for SystemTree {
    type Node = AxElement;

    fn focused_element(&mut self) -> Option<AxElement> {
        let system = unsafe { AxElement::from_created(AXUIElementCreateSystemWide()) }?;
        let focused = element_for_pid(
            self.read(&system, "AXFocusedUIElement", false).ok()??,
            self.pid,
        )?;
        self.within_deadline().then_some(focused)
    }

    fn role(&mut self, node: &AxElement) -> Option<Role> {
        metadata_role(self.read(node, "AXRole", false).ok()??)
    }

    fn subrole(&mut self, node: &AxElement) -> Result<(), ()> {
        metadata_subrole(self.read(node, "AXSubrole", true)?)
    }

    fn children(&mut self, node: &AxElement, limit: usize) -> Vec<AxElement> {
        if limit == 0 {
            return Vec::new();
        }
        if node == &self.application {
            // Only the window of the app active at recording start.
            return self
                .read(node, "AXFocusedWindow", false)
                .ok()
                .flatten()
                .and_then(|value| element_for_pid(value, self.pid))
                .into_iter()
                .collect();
        }
        let attribute = CFString::from_static_string("AXChildren");
        if !self.bound_call(node) {
            return Vec::new();
        }
        // Request a bounded slice, rather than allocating the complete children
        // array of an arbitrarily wide application tree.
        let mut values: CFArrayRef = null();
        let status = unsafe {
            AXUIElementCopyAttributeValues(
                node.raw(),
                attribute.as_concrete_TypeRef(),
                0,
                limit.min(MAX_NODES) as CFIndex,
                &mut values,
            )
        };
        let values =
            (!values.is_null()).then(|| unsafe { CFType::wrap_under_create_rule(values.cast()) });
        if status != 0 || !self.within_deadline() {
            return Vec::new();
        }
        let Some(values) = values.and_then(|values| values.downcast_into::<CFArray>()) else {
            return Vec::new();
        };
        values
            .iter()
            .take(limit)
            .filter_map(|value| {
                (!(*value).is_null())
                    .then(|| unsafe { CFType::wrap_under_get_rule(*value) })
                    .and_then(|value| element_for_pid(value, self.pid))
            })
            .collect()
    }

    fn within_deadline(&self) -> bool {
        Instant::now() < self.deadline
    }
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXUIElementCreateApplication(pid: libc::pid_t) -> CFTypeRef;
    fn AXUIElementCreateSystemWide() -> CFTypeRef;
    fn AXUIElementGetTypeID() -> CFTypeID;
    fn AXUIElementGetPid(element: CFTypeRef, pid: *mut libc::pid_t) -> i32;
    fn AXUIElementSetMessagingTimeout(element: CFTypeRef, timeout: f32) -> i32;
    fn AXUIElementCopyAttributeValue(
        element: CFTypeRef,
        attribute: CFStringRef,
        value: *mut CFTypeRef,
    ) -> i32;
    fn AXUIElementCopyAttributeValues(
        element: CFTypeRef,
        attribute: CFStringRef,
        index: CFIndex,
        max_values: CFIndex,
        values: *mut CFArrayRef,
    ) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_or_unbundled_application_is_selected_by_pid_alone() {
        let mut requested = None;
        dispatch_preparation(Some(41), |pid| requested = Some(pid));
        assert_eq!(requested, Some(41));
    }

    #[test]
    fn absent_or_invalid_frontmost_process_is_not_dispatched() {
        for pid in [None, Some(0), Some(-1)] {
            dispatch_preparation(pid, |_| panic!("invalid process must not be dispatched"));
        }
    }

    // Deterministic lazy-AX model, not evidence of real application behavior.
    // Native and web container metadata reads expose the simulated focus.
    struct LazyApplication {
        native_enabled: bool,
        web_enabled: bool,
        reads: usize,
        deadline: bool,
        read_budget: usize,
        cyclic: bool,
    }

    impl LazyApplication {
        fn cold() -> Self {
            Self {
                native_enabled: false,
                web_enabled: false,
                reads: 0,
                deadline: true,
                read_budget: usize::MAX,
                cyclic: false,
            }
        }

        fn focused_field(&self) -> Option<&str> {
            self.web_enabled.then_some("AXTextArea")
        }
    }

    impl AccessibilityTree for LazyApplication {
        type Node = u32;

        fn focused_element(&mut self) -> Option<u32> {
            self.focused_field().map(|_| 9)
        }

        fn subrole(&mut self, _: &u32) -> Result<(), ()> {
            Ok(())
        }

        fn role(&mut self, node: &u32) -> Option<Role> {
            if *node == 9 {
                return Some(Role::Other);
            }
            self.reads += 1;
            let role = match node {
                0 => {
                    self.native_enabled = true;
                    "AXApplication"
                }
                1 => "AXWindow",
                2 => "AXToolbar",
                3 if self.native_enabled => {
                    self.web_enabled = true;
                    "AXScrollArea"
                }
                4 if self.web_enabled => "AXWebArea",
                _ => return None,
            };
            Some(if role == "AXWebArea" {
                Role::WebArea
            } else {
                Role::Other
            })
        }

        fn children(&mut self, node: &u32, limit: usize) -> Vec<u32> {
            assert!(limit <= 128);
            self.reads += 1;
            if self.cyclic {
                return vec![*node];
            }
            match node {
                0 if self.native_enabled => vec![1],
                1 => vec![2, 3],
                3 if self.web_enabled => vec![4],
                // Reading content below the web root is unnecessary.
                4 => panic!("must not traverse page content"),
                _ => vec![],
            }
        }

        fn within_deadline(&self) -> bool {
            self.deadline && self.reads < self.read_budget
        }
    }

    #[test]
    fn cold_application_exposes_focused_field_after_native_and_web_role_requests() {
        let mut application = LazyApplication::cold();
        assert_eq!(application.focused_field(), None);

        assert_eq!(
            prepare_tree(&mut application, 0),
            PreparationOutcome::ReadyAfterPriming
        );

        assert_eq!(application.focused_field(), Some("AXTextArea"));
    }

    #[test]
    fn ready_focus_skips_tree_traversal() {
        let mut application = LazyApplication::cold();
        application.native_enabled = true;
        application.web_enabled = true;

        let outcome = prepare_tree(&mut application, 0);
        assert_eq!(
            outcome,
            if application.deadline {
                PreparationOutcome::AlreadyReady
            } else {
                PreparationOutcome::BudgetExhausted
            }
        );

        assert_eq!(application.reads, 0);
    }

    #[test]
    fn expired_preparation_does_not_read_the_application() {
        let mut application = LazyApplication::cold();
        application.deadline = false;

        let outcome = prepare_tree(&mut application, 0);
        assert_eq!(
            outcome,
            if application.deadline {
                PreparationOutcome::AlreadyReady
            } else {
                PreparationOutcome::BudgetExhausted
            }
        );

        assert_eq!(application.reads, 0);
        assert_eq!(application.focused_field(), None);
    }

    #[test]
    fn cyclic_native_tree_has_a_bounded_walk() {
        let mut application = LazyApplication::cold();
        application.cyclic = true;

        prepare_tree(&mut application, 0);

        assert_eq!(application.reads, 2);
        assert_eq!(application.focused_field(), None);
    }

    #[test]
    fn preparation_stops_when_deadline_expires_during_a_walk() {
        let mut application = LazyApplication::cold();
        application.read_budget = 3;

        prepare_tree(&mut application, 0);

        assert_eq!(application.reads, 3);
        assert_eq!(application.focused_field(), None);
    }

    #[test]
    fn wide_native_tree_cannot_exceed_the_node_budget() {
        struct WideTree(usize);
        impl AccessibilityTree for WideTree {
            type Node = u32;
            fn focused_element(&mut self) -> Option<u32> {
                None
            }
            fn subrole(&mut self, _: &u32) -> Result<(), ()> {
                Err(())
            }
            fn role(&mut self, _: &u32) -> Option<Role> {
                self.0 += 1;
                None
            }
            fn children(&mut self, node: &u32, limit: usize) -> Vec<u32> {
                if *node == 0 {
                    (1..=256).take(limit).collect()
                } else {
                    vec![]
                }
            }
            fn within_deadline(&self) -> bool {
                true
            }
        }
        let mut tree = WideTree(0);

        assert_eq!(
            prepare_tree(&mut tree, 0),
            PreparationOutcome::BudgetExhausted
        );

        assert_eq!(tree.0, 128);
    }
    fn string_value(value: &str) -> CFType {
        CFString::new(value).as_CFType()
    }

    #[test]
    fn required_and_optional_attribute_errors_match_inserter_contract() {
        for status in [-25205, -25212] {
            assert!(attribute_result(status, None, true).unwrap().is_none());
            assert!(attribute_result(status, None, false).is_err());
            assert!(attribute_result(status, Some(string_value("AXTextField")), true).is_err());
        }
        for optional in [false, true] {
            assert!(attribute_result(0, None, optional).is_err());
            assert!(attribute_result(-25204, None, optional).is_err());
            assert!(attribute_result(0, Some(string_value("AXTextField")), optional).is_ok());
        }
    }

    #[test]
    fn focus_must_be_an_ax_element_from_the_captured_process() {
        assert!(element_for_pid(string_value("not an element"), 41).is_none());
        // Creating AX application objects and reading their PIDs is local;
        // this test does not query, activate, or control another application.
        let element = unsafe { AxElement::from_created(AXUIElementCreateApplication(41)) }.unwrap();
        assert!(element_for_pid(element.0.clone(), 41).is_some());
        assert!(element_for_pid(element.0, 42).is_none());
    }

    #[test]
    fn metadata_decoders_require_strings_and_accept_secure_field_metadata() {
        let invalid = unsafe { AxElement::from_created(AXUIElementCreateApplication(41)) }.unwrap();
        assert_eq!(metadata_role(invalid.0.clone()), None);
        assert!(metadata_subrole(Some(invalid.0)).is_err());
        assert_eq!(
            metadata_role(string_value("AXWebArea")),
            Some(Role::WebArea)
        );
        assert_eq!(
            metadata_role(string_value("AXSecureTextField")),
            Some(Role::Other)
        );
        assert!(metadata_subrole(Some(string_value("AXSecureTextField"))).is_ok());
        assert!(metadata_subrole(None).is_ok());
    }

    struct ProbeTree {
        role_available: bool,
        subrole_available: bool,
        calls: usize,
        expire_after: usize,
        tree_reads: usize,
        cold: bool,
    }

    impl ProbeTree {
        fn ready() -> Self {
            Self {
                role_available: true,
                subrole_available: true,
                calls: 0,
                expire_after: usize::MAX,
                tree_reads: 0,
                cold: false,
            }
        }
    }

    impl AccessibilityTree for ProbeTree {
        type Node = u32;
        fn focused_element(&mut self) -> Option<u32> {
            self.calls += 1;
            if self.cold && self.calls == 1 {
                None
            } else {
                Some(9)
            }
        }
        fn role(&mut self, node: &u32) -> Option<Role> {
            self.calls += 1;
            if *node != 9 {
                self.tree_reads += 1;
            }
            self.role_available.then_some(Role::Other)
        }
        fn subrole(&mut self, _: &u32) -> Result<(), ()> {
            self.calls += 1;
            if self.subrole_available {
                Ok(())
            } else {
                Err(())
            }
        }
        fn children(&mut self, _: &u32, _: usize) -> Vec<u32> {
            self.tree_reads += 1;
            vec![]
        }
        fn within_deadline(&self) -> bool {
            self.calls < self.expire_after
        }
    }

    #[test]
    fn late_focus_role_or_subrole_success_cannot_report_ready() {
        for expire_after in [1, 2, 3] {
            let mut tree = ProbeTree {
                expire_after,
                ..ProbeTree::ready()
            };
            assert_eq!(
                prepare_tree(&mut tree, 0),
                PreparationOutcome::BudgetExhausted
            );
            assert_eq!(tree.calls, expire_after);
            assert_eq!(tree.tree_reads, 0);
        }
    }

    #[test]
    fn missing_role_or_real_subrole_error_remains_unavailable_after_priming() {
        for (role_available, subrole_available) in [(false, true), (true, false)] {
            let mut tree = ProbeTree {
                role_available,
                subrole_available,
                ..ProbeTree::ready()
            };
            assert_eq!(prepare_tree(&mut tree, 0), PreparationOutcome::Unavailable);
            assert_eq!(tree.tree_reads, 2);
        }
    }

    #[test]
    fn call_timeout_is_capped_by_remaining_overall_budget() {
        let now = Instant::now();
        assert_eq!(
            remaining_call_budget(now + PREPARATION_BUDGET, now),
            Some(CALL_BUDGET)
        );
        assert_eq!(
            remaining_call_budget(now + Duration::from_millis(7), now),
            Some(Duration::from_millis(7))
        );
        assert_eq!(remaining_call_budget(now, now), None);
        assert_eq!(
            remaining_call_budget(now, now + Duration::from_nanos(1)),
            None
        );
    }

    #[test]
    fn singleflight_releases_on_return_unwind_and_rejected_spawn() {
        let busy = AtomicBool::new(false);
        let guard = AxProbeLease::acquire(&busy).unwrap();
        assert!(AxProbeLease::acquire(&busy).is_none());
        drop(guard);
        assert!(!busy.load(Ordering::Acquire));
        let panic = std::panic::catch_unwind(|| {
            let _guard = AxProbeLease::acquire(&busy).unwrap();
            panic!("simulated worker panic");
        });
        assert!(panic.is_err());
        assert!(!busy.load(Ordering::Acquire));
        fn failed_spawn(_work: impl FnOnce()) -> std::io::Result<()> {
            Err(std::io::Error::other("simulated thread creation failure"))
        }
        let guard = AxProbeLease::acquire(&busy).unwrap();
        assert!(failed_spawn(move || {
            let _guard = guard;
        })
        .is_err());
        assert!(!busy.load(Ordering::Acquire));
        assert!(AxProbeLease::acquire(&busy).is_some());
    }

    #[test]
    fn depth_limit_is_bounded_and_reported_as_budget_exhausted() {
        struct DeepTree(usize);
        impl AccessibilityTree for DeepTree {
            type Node = usize;
            fn focused_element(&mut self) -> Option<usize> {
                None
            }
            fn role(&mut self, node: &usize) -> Option<Role> {
                assert!(*node <= MAX_DEPTH);
                self.0 += 1;
                Some(Role::Other)
            }
            fn subrole(&mut self, _: &usize) -> Result<(), ()> {
                Err(())
            }
            fn children(&mut self, node: &usize, _: usize) -> Vec<usize> {
                vec![node + 1]
            }
            fn within_deadline(&self) -> bool {
                true
            }
        }
        let mut tree = DeepTree(0);
        assert_eq!(
            prepare_tree(&mut tree, 0),
            PreparationOutcome::BudgetExhausted
        );
        assert_eq!(tree.0, MAX_DEPTH + 1);
    }
    #[test]
    fn late_recheck_after_priming_cannot_report_ready() {
        for expire_after in [3, 4, 5] {
            let mut tree = ProbeTree {
                cold: true,
                expire_after,
                ..ProbeTree::ready()
            };
            assert_eq!(
                prepare_tree(&mut tree, 0),
                PreparationOutcome::BudgetExhausted
            );
            assert_eq!(tree.calls, expire_after);
            assert_eq!(tree.tree_reads, 2);
        }
    }

    #[test]
    fn secure_role_or_subrole_is_ready_without_priming_or_paste_authorization() {
        struct SecureTree {
            role: CFType,
            subrole: CFType,
        }
        impl AccessibilityTree for SecureTree {
            type Node = u32;
            fn focused_element(&mut self) -> Option<u32> {
                Some(9)
            }
            fn role(&mut self, node: &u32) -> Option<Role> {
                assert_eq!(*node, 9, "must not walk the application");
                metadata_role(self.role.clone())
            }
            fn subrole(&mut self, _: &u32) -> Result<(), ()> {
                metadata_subrole(Some(self.subrole.clone()))
            }
            fn children(&mut self, _: &u32, _: usize) -> Vec<u32> {
                panic!("must not prime ready secure metadata")
            }
            fn within_deadline(&self) -> bool {
                true
            }
        }
        for (role, subrole) in [
            ("AXSecureTextField", ""),
            ("AXTextField", "AXSecureTextField"),
        ] {
            let mut tree = SecureTree {
                role: string_value(role),
                subrole: string_value(subrole),
            };
            assert_eq!(prepare_tree(&mut tree, 0), PreparationOutcome::AlreadyReady);
        }
    }
}
