//! Coalesced cross-thread notification of one main-runloop callback.
use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::ops::Deref;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::rc::Rc;
use std::sync::{mpsc::Sender, Arc, Mutex};

use core_foundation::base::{kCFAllocatorDefault, TCFType};
use core_foundation::runloop::*;

struct WakeTarget {
    source: CFRunLoopSourceRef,
    run_loop: CFRunLoopRef,
}
// Only CF's thread-safe signal/wake operations use these borrowed references,
// under WakeState's mutex. close removes the target under that same lock before
// either owner is invalidated/released. No Runtime pointer crosses threads.
unsafe impl Send for WakeTarget {}

#[derive(Default)]
struct WakeState {
    target: Option<WakeTarget>,
    pending: bool,
}

#[derive(Clone, Default)]
pub(crate) struct EventNotifier(Arc<Mutex<WakeState>>);
impl EventNotifier {
    pub(crate) fn notify(&self) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if state.pending || state.target.is_none() {
            return;
        }
        state.pending = true;
        let target = state.target.as_ref().unwrap();
        unsafe {
            CFRunLoopSourceSignal(target.source);
            CFRunLoopWakeUp(target.run_loop);
        }
    }
    pub(crate) fn send<T>(&self, sender: &Sender<T>, value: T) -> bool {
        let sent = sender.send(value).is_ok();
        if sent {
            self.notify();
        }
        sent
    }
    fn clear_pending(&self) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).pending = false;
    }
}

/// Explicit sender destruction before terminal notification, including unwind.
/// A terminal wake guarantees channel disconnection, not JoinHandle completion.
pub(crate) struct TerminalSender<T> {
    sender: Option<T>,
    notifier: EventNotifier,
}
impl<T> TerminalSender<T> {
    pub(crate) fn new(sender: T, notifier: EventNotifier) -> Self {
        Self {
            sender: Some(sender),
            notifier,
        }
    }
}
impl<T> Deref for TerminalSender<T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.sender.as_ref().unwrap()
    }
}
impl<T> Drop for TerminalSender<T> {
    fn drop(&mut self) {
        drop(self.sender.take());
        self.notifier.notify();
    }
}

struct Callback {
    notifier: EventNotifier,
    busy: Cell<bool>,
    closed: Cell<bool>,
    deferred: Cell<bool>,
    handler: RefCell<Box<dyn FnMut()>>,
}
pub(crate) struct CallbackGuard<'a>(&'a Callback);
impl Drop for CallbackGuard<'_> {
    fn drop(&mut self) {
        self.0.busy.set(false);
        if self.0.closed.get() {
            // The invocation's RefMut is gone before this guard. This also
            // clears a raw Runtime capture when close occurred in the callback.
            *self.0.handler.borrow_mut() = Box::new(|| {});
            self.0.deferred.set(false);
            return;
        }
        if self.0.deferred.replace(false) || std::thread::panicking() {
            self.0.notifier.notify();
        }
    }
}
impl Callback {
    fn perform(&self) {
        // Clear BEFORE queue inspection. A producer on either side is covered.
        self.notifier.clear_pending();
        if self.closed.get() {
            return;
        }
        if self.busy.replace(true) {
            self.deferred.set(true);
            return;
        }
        let _guard = CallbackGuard(self);
        (self.handler.borrow_mut())();
    }
}
extern "C" fn perform(info: *const c_void) {
    let result = catch_unwind(AssertUnwindSafe(|| unsafe {
        // CF retains the context. Take a local owner before calling arbitrary
        // code, allowing source cancellation during its own callback.
        let ptr = info.cast::<Callback>();
        Rc::increment_strong_count(ptr);
        let callback = Rc::from_raw(ptr);
        callback.perform();
    }));
    if result.is_err() {
        tracing::error!(error_category = "event_callback_panic");
    }
}
extern "C" fn retain(info: *const c_void) -> *const c_void {
    unsafe { Rc::increment_strong_count(info.cast::<Callback>()) };
    info
}
extern "C" fn release(info: *const c_void) {
    unsafe { drop(Rc::from_raw(info.cast::<Callback>())) };
}

pub(crate) struct EventSource {
    source: CFRunLoopSource,
    run_loop: CFRunLoop,
    callback: Rc<Callback>,
}
impl EventSource {
    /// Creates but does not register a source. Install the handler and finish
    /// all Runtime initialization before attach, with no pointee borrow alive.
    pub(crate) fn new(run_loop: CFRunLoop) -> Self {
        let callback = Rc::new(Callback {
            notifier: EventNotifier::default(),
            busy: Cell::new(false),
            closed: Cell::new(false),
            deferred: Cell::new(false),
            handler: RefCell::new(Box::new(|| {})),
        });
        let mut context = CFRunLoopSourceContext {
            version: 0,
            info: Rc::as_ptr(&callback).cast_mut().cast(),
            retain: Some(retain),
            release: Some(release),
            copyDescription: None,
            equal: None,
            hash: None,
            schedule: None,
            cancel: None,
            perform,
        };
        let raw = unsafe { CFRunLoopSourceCreate(kCFAllocatorDefault, 0, &mut context) };
        assert!(!raw.is_null(), "main event source creation failed");
        let source = unsafe { CFRunLoopSource::wrap_under_create_rule(raw) };
        callback.notifier.0.lock().unwrap().target = Some(WakeTarget {
            source: raw,
            run_loop: run_loop.as_concrete_TypeRef(),
        });
        Self {
            source,
            run_loop,
            callback,
        }
    }
    // Owner-handle shutdown access uses the same guard BEFORE creating a
    // pointee reference, since native restoration may pump a nested loop.
    pub(crate) fn suspend(&self) -> CallbackGuard<'_> {
        assert!(
            !self.callback.busy.replace(true),
            "nested Runtime owner access"
        );
        CallbackGuard(&self.callback)
    }
    pub(crate) fn notifier(&self) -> EventNotifier {
        self.callback.notifier.clone()
    }
    pub(crate) fn set_handler(&self, handler: impl FnMut() + 'static) {
        *self.callback.handler.borrow_mut() = Box::new(handler);
    }
    pub(crate) fn attach(&self) {
        self.run_loop
            .add_source(&self.source, unsafe { kCFRunLoopCommonModes });
        self.notifier().notify();
    }
    pub(crate) fn close(&self) {
        self.callback.closed.set(true);
        {
            let mut state = self
                .callback
                .notifier
                .0
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            state.target = None;
            state.pending = false;
        }
        unsafe { CFRunLoopSourceInvalidate(self.source.as_concrete_TypeRef()) };
        self.run_loop
            .remove_source(&self.source, unsafe { kCFRunLoopCommonModes });
        if !self.callback.busy.get() {
            *self.callback.handler.borrow_mut() = Box::new(|| {});
        }
    }
}
impl Drop for EventSource {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::mpsc::{self, TryRecvError};
    use std::sync::Barrier;
    use std::thread;
    use std::time::{Duration, Instant};

    pub(crate) fn pump_until(mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !done() {
            assert!(Instant::now() < deadline, "event source did not deliver");
            CFRunLoop::run_in_mode(
                unsafe { kCFRunLoopDefaultMode },
                Duration::from_millis(10),
                true,
            );
        }
    }

    #[test]
    fn burst_before_attach_coalesces_and_delivers_on_owning_common_mode() {
        let source = EventSource::new(CFRunLoop::get_current());
        let notifier = source.notifier();
        let calls = Rc::new(Cell::new(0));
        let seen = calls.clone();
        let owner = thread::current().id();
        source.set_handler(move || {
            assert_eq!(thread::current().id(), owner);
            seen.set(seen.get() + 1);
        });
        thread::spawn(move || {
            for _ in 0..1_000 {
                notifier.notify();
            }
        })
        .join()
        .unwrap();
        source.attach();
        // A separate common mode models AppKit menu tracking without AppKit.
        let mode = core_foundation::string::CFString::new("PTT2meTestTracking");
        unsafe {
            CFRunLoopAddCommonMode(
                source.run_loop.as_concrete_TypeRef(),
                mode.as_concrete_TypeRef(),
            )
        };
        CFRunLoop::run_in_mode(mode.as_concrete_TypeRef(), Duration::from_millis(100), true);
        assert_eq!(calls.get(), 1);
        CFRunLoop::run_in_mode(mode.as_concrete_TypeRef(), Duration::ZERO, true);
        assert_eq!(
            calls.get(),
            1,
            "healthy idle has no self-resignalling callback"
        );
    }

    #[test]
    fn producers_before_during_and_after_pending_clear_are_all_delivered() {
        let source = EventSource::new(CFRunLoop::get_current());
        let (sender, receiver) = mpsc::channel();
        let (cleared, may_send) = mpsc::channel();
        let (sent, enqueued) = mpsc::channel();
        let (after, may_send_after) = mpsc::channel();
        let notifier = source.notifier();
        notifier.send(&sender, 1);
        let worker = thread::spawn(move || {
            may_send.recv().unwrap();
            notifier.send(&sender, 2);
            sent.send(()).unwrap();
            may_send_after.recv().unwrap();
            notifier.send(&sender, 3);
        });
        let values = Rc::new(RefCell::new(Vec::new()));
        let seen = values.clone();
        let mut first = true;
        source.set_handler(move || {
            if first {
                first = false;
                seen.borrow_mut().push(receiver.try_recv().unwrap());
                cleared.send(()).unwrap();
                enqueued.recv().unwrap();
            }
            while let Ok(value) = receiver.try_recv() {
                seen.borrow_mut().push(value);
            }
        });
        source.attach();
        pump_until(|| values.borrow().len() == 2);
        after.send(()).unwrap();
        worker.join().unwrap();
        pump_until(|| values.borrow().len() == 3);
        assert_eq!(*values.borrow(), [1, 2, 3]);
    }

    #[test]
    fn concurrent_close_makes_late_notifiers_inert() {
        let source = EventSource::new(CFRunLoop::get_current());
        let notifier = source.notifier();
        let barrier = Arc::new(Barrier::new(2));
        let start = barrier.clone();
        let late = notifier.clone();
        let worker = thread::spawn(move || {
            start.wait();
            for _ in 0..10_000 {
                late.notify();
            }
        });
        source.set_handler(|| panic!("closed source executed"));
        source.attach();
        barrier.wait();
        source.close();
        worker.join().unwrap();
        let weak = Rc::downgrade(&source.callback);
        drop(source);
        notifier.notify();
        assert!(notifier.0.lock().unwrap().target.is_none());
        assert!(
            weak.upgrade().is_none(),
            "notifier retained callback ownership"
        );
        CFRunLoop::run_in_mode(unsafe { kCFRunLoopDefaultMode }, Duration::ZERO, true);
    }

    #[test]
    fn terminal_sender_is_destroyed_before_wake_on_success_and_panic() {
        for panic in [false, true] {
            let source = EventSource::new(CFRunLoop::get_current());
            let (sender, receiver) = mpsc::channel::<()>();
            let notifier = source.notifier();
            let worker = thread::spawn(move || {
                let _sender = TerminalSender::new(sender, notifier);
                if panic {
                    panic!("synthetic terminal failure");
                }
            });
            assert_eq!(worker.join().is_err(), panic);
            let disconnected = Rc::new(Cell::new(false));
            let seen = disconnected.clone();
            source.set_handler(move || {
                assert_eq!(receiver.try_recv(), Err(TryRecvError::Disconnected));
                seen.set(true);
            });
            source.attach();
            pump_until(|| disconnected.get());
        }
    }

    #[test]
    fn nested_source_defers_once_and_panic_releases_guard() {
        let source = Rc::new(EventSource::new(CFRunLoop::get_current()));
        let weak = Rc::downgrade(&source);
        let calls = Rc::new(Cell::new(0));
        let seen = calls.clone();
        source.set_handler(move || {
            seen.set(seen.get() + 1);
            if seen.get() == 1 {
                let nested = weak.upgrade().unwrap();
                for _ in 0..10 {
                    nested.callback.perform();
                }
                assert_eq!(seen.get(), 1);
                assert!(
                    !nested.notifier().0.lock().unwrap().pending,
                    "nested callback must not spin"
                );
                panic!("synthetic callback panic");
            }
        });
        source.attach();
        pump_until(|| calls.get() == 2);
        assert!(!source.callback.busy.get());
        assert!(!source.callback.deferred.get());
    }

    #[test]
    fn caught_panic_reschedules_work_even_without_nested_callback() {
        let source = EventSource::new(CFRunLoop::get_current());
        let calls = Rc::new(Cell::new(0));
        let seen = calls.clone();
        source.set_handler(move || {
            seen.set(seen.get() + 1);
            if seen.get() == 1 {
                panic!("synthetic handler failure with queued work");
            }
        });
        source.attach();
        CFRunLoop::run_in_mode(
            unsafe { kCFRunLoopDefaultMode },
            Duration::from_millis(100),
            true,
        );
        assert!(!source.callback.busy.get());
        assert!(
            source.notifier().0.lock().unwrap().pending,
            "caught panic lost queued continuation"
        );
        pump_until(|| calls.get() == 2);
    }

    #[test]
    fn callback_can_invalidate_its_own_native_source() {
        let source = Rc::new(EventSource::new(CFRunLoop::get_current()));
        let weak = Rc::downgrade(&source);
        let closed = Rc::new(Cell::new(false));
        let seen = closed.clone();
        source.set_handler(move || {
            weak.upgrade().unwrap().close();
            seen.set(true);
        });
        source.attach();
        pump_until(|| closed.get());
        assert!(!source.callback.busy.get());
        source.notifier().notify();
        assert!(!source.notifier().0.lock().unwrap().pending);
    }
}
