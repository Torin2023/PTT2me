//! Warm up supported Chromium apps' lazy accessibility trees before recognition finishes.

use std::ffi::c_void;
use std::ptr::null;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use core_foundation::array::CFArray;
use core_foundation::base::{CFType, CFTypeID, CFTypeRef, TCFType};
use core_foundation::string::{CFString, CFStringRef};
use objc2_app_kit::NSWorkspace;

const MAX_NODES: usize = 128;
const MAX_DEPTH: usize = 12;
static PREPARING: AtomicBool = AtomicBool::new(false);

/// Chromium 152 enables native AX on application AXRole, and web AX on the web
/// contents container AXRole. A system-wide AXFocusedUIElement query alone does
/// neither. Start the read-only warm-up while audio is being captured so the
/// supported app has time to publish its renderer tree. Never move focus or use
/// a discovered node as the insertion target; text_inserter still checks the
/// current system focus and secure status at both begin and paste time.
pub(crate) fn prepare_focused_browser() {
    let Some(application) = (unsafe { NSWorkspace::sharedWorkspace().frontmostApplication() })
    else {
        return;
    };
    let Some(identifier) = (unsafe { application.bundleIdentifier() }) else {
        return;
    };
    dispatch_preparation(&identifier.to_string(), || {
        let pid: libc::pid_t = unsafe { objc2::msg_send![&*application, processIdentifier] };
        request_preparation(pid);
    });
}

fn dispatch_preparation<F>(identifier: &str, request: F)
where
    F: FnOnce(),
{
    if is_supported_chromium_app(identifier) {
        request();
    }
}

fn request_preparation(pid: libc::pid_t) {
    if PREPARING.swap(true, Ordering::AcqRel) {
        return;
    }
    struct PreparationGuard;
    impl Drop for PreparationGuard {
        fn drop(&mut self) {
            PREPARING.store(false, Ordering::Release);
        }
    }
    let guard = PreparationGuard;
    // AX IPC must not hold the audio/menu run loop. A single bounded worker
    // also prevents rapid PTT presses from accumulating background requests.
    let _ = std::thread::Builder::new()
        .name("browser-accessibility".into())
        .spawn(move || {
            let _guard = guard;
            let raw = unsafe { AXUIElementCreateApplication(pid) };
            if raw.is_null() {
                return;
            }
            let root = unsafe { CFType::wrap_under_create_rule(raw) };
            let mut tree = SystemTree {
                application: root.as_CFTypeRef(),
                deadline: Instant::now() + Duration::from_millis(500),
            };
            prime_tree(&mut tree, root);
            tracing::debug!(lifecycle = "browser_accessibility_requested");
        });
}

fn is_supported_chromium_app(identifier: &str) -> bool {
    matches!(
        identifier,
        "com.google.Chrome"
            | "com.google.Chrome.beta"
            | "com.google.Chrome.dev"
            | "com.google.Chrome.canary"
            | "com.openai.codex"
    )
}

trait AccessibilityTree {
    type Node;

    fn role(&mut self, node: &Self::Node) -> Option<String>;
    fn children(&mut self, node: &Self::Node) -> Vec<Self::Node>;
    fn within_deadline(&self) -> bool;
}

fn prime_tree<T: AccessibilityTree>(tree: &mut T, root: T::Node) {
    let mut pending = vec![(root, 0)];
    for _ in 0..MAX_NODES {
        let Some((node, depth)) = pending.pop() else {
            break;
        };
        if !tree.within_deadline() {
            break;
        }
        // Do not inspect text, values, selections, URLs, or page descendants.
        if tree.role(&node).as_deref() == Some("AXWebArea") {
            break;
        }
        if depth < MAX_DEPTH && tree.within_deadline() {
            let remaining = MAX_NODES.saturating_sub(pending.len());
            let children = tree.children(&node);
            pending.extend(
                children
                    .into_iter()
                    .take(remaining)
                    .map(|child| (child, depth + 1))
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev(),
            );
        }
    }
}

struct SystemTree {
    application: CFTypeRef,
    deadline: Instant,
}

impl SystemTree {
    fn read(&self, node: &CFType, attribute: &'static str) -> Option<CFType> {
        if !self.within_deadline() || node.type_of() != unsafe { AXUIElementGetTypeID() } {
            return None;
        }
        let attribute = CFString::from_static_string(attribute);
        let mut value: CFTypeRef = null();
        unsafe {
            if AXUIElementSetMessagingTimeout(node.as_CFTypeRef(), 0.05) != 0 {
                return None;
            }
            let status = AXUIElementCopyAttributeValue(
                node.as_CFTypeRef(),
                attribute.as_concrete_TypeRef(),
                &mut value,
            );
            let value = (!value.is_null()).then(|| CFType::wrap_under_create_rule(value));
            if status == 0 {
                value
            } else {
                None
            }
        }
    }
}

impl AccessibilityTree for SystemTree {
    type Node = CFType;

    fn role(&mut self, node: &CFType) -> Option<String> {
        self.read(node, "AXRole")?
            .downcast_into::<CFString>()
            .map(|role| role.to_string())
    }

    fn children(&mut self, node: &CFType) -> Vec<CFType> {
        if node.as_CFTypeRef() == self.application {
            // Only the window belonging to the app active at capture start.
            return self.read(node, "AXFocusedWindow").into_iter().collect();
        }
        let Some(children) = self
            .read(node, "AXChildren")
            .and_then(|value| value.downcast_into::<CFArray>())
        else {
            return Vec::new();
        };
        children
            .iter()
            .take(MAX_NODES)
            .filter_map(|child| {
                (!(*child).is_null()).then(|| unsafe { CFType::wrap_under_get_rule(*child) })
            })
            .collect()
    }

    fn within_deadline(&self) -> bool {
        Instant::now() < self.deadline
    }
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXUIElementCreateApplication(pid: libc::pid_t) -> *const c_void;
    fn AXUIElementGetTypeID() -> CFTypeID;
    fn AXUIElementSetMessagingTimeout(element: CFTypeRef, timeout: f32) -> i32;
    fn AXUIElementCopyAttributeValue(
        element: CFTypeRef,
        attribute: CFStringRef,
        value: *mut CFTypeRef,
    ) -> i32;
}

#[cfg(test)]
mod tests {
    use super::{dispatch_preparation, prime_tree, AccessibilityTree};

    #[test]
    fn supported_chromium_apps_request_accessibility_preparation() {
        for identifier in [
            "com.google.Chrome",
            "com.google.Chrome.beta",
            "com.google.Chrome.dev",
            "com.google.Chrome.canary",
            "com.openai.codex",
        ] {
            let mut requested = false;

            dispatch_preparation(identifier, || requested = true);

            assert!(requested, "bundle ID: {identifier}");
        }
    }

    #[test]
    fn unrelated_apps_do_not_request_accessibility_preparation() {
        let mut requested = false;

        dispatch_preparation("com.example.native-editor", || requested = true);

        assert!(!requested);
    }

    // Mirrors Chrome 152: application AXRole enables native accessibility;
    // the web contents container AXRole enables renderer accessibility.
    struct LazyBrowser {
        native_enabled: bool,
        web_enabled: bool,
        reads: usize,
        deadline: bool,
        read_budget: usize,
        cyclic: bool,
    }

    impl LazyBrowser {
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

    impl AccessibilityTree for LazyBrowser {
        type Node = u32;

        fn role(&mut self, node: &u32) -> Option<String> {
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
            Some(role.to_owned())
        }

        fn children(&mut self, node: &u32) -> Vec<u32> {
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
    fn cold_browser_exposes_focused_field_after_native_and_web_role_requests() {
        let mut browser = LazyBrowser::cold();
        assert_eq!(browser.focused_field(), None);

        prime_tree(&mut browser, 0);

        assert_eq!(browser.focused_field(), Some("AXTextArea"));
    }

    #[test]
    fn expired_preparation_does_not_read_the_application() {
        let mut browser = LazyBrowser::cold();
        browser.deadline = false;

        prime_tree(&mut browser, 0);

        assert_eq!(browser.reads, 0);
        assert_eq!(browser.focused_field(), None);
    }

    #[test]
    fn cyclic_native_tree_has_a_bounded_walk() {
        let mut browser = LazyBrowser::cold();
        browser.cyclic = true;

        prime_tree(&mut browser, 0);

        assert!(browser.reads > 0 && browser.reads <= 256);
        assert_eq!(browser.focused_field(), None);
    }

    #[test]
    fn preparation_stops_when_deadline_expires_during_a_walk() {
        let mut browser = LazyBrowser::cold();
        browser.read_budget = 3;

        prime_tree(&mut browser, 0);

        assert_eq!(browser.reads, 3);
        assert_eq!(browser.focused_field(), None);
    }

    #[test]
    fn wide_native_tree_cannot_exceed_the_node_budget() {
        struct WideTree(usize);
        impl AccessibilityTree for WideTree {
            type Node = u32;
            fn role(&mut self, _: &u32) -> Option<String> {
                self.0 += 1;
                None
            }
            fn children(&mut self, node: &u32) -> Vec<u32> {
                if *node == 0 {
                    (1..=256).collect()
                } else {
                    vec![]
                }
            }
            fn within_deadline(&self) -> bool {
                true
            }
        }
        let mut tree = WideTree(0);

        prime_tree(&mut tree, 0);

        assert_eq!(tree.0, 128);
    }
}
