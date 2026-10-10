//! A minimal main-thread event queue for the tray shell: background threads
//! (the poller, the action threads) push messages, the main run loop drains
//! them through a `CFRunLoopSource`.
//!
//! The source is added to `kCFRunLoopDefaultMode` ONLY — deliberately not
//! `kCFRunLoopCommonModes`. AppKit tracks an open NSMenu in
//! `NSEventTrackingRunLoopMode`, which is a common mode, so anything
//! registered in the common modes fires WHILE the menu is open — and waking
//! the loop inside menu tracking dismisses the menu on recent macOS
//! (rust-windowing/tao#1324; winit's proxy source and control-flow observers
//! are registered in the common modes, which is why the tray menu used to
//! close itself within a poll of every click). With a default-mode-only
//! source, queued messages simply wait while the menu is open and are applied
//! the moment it closes. `NSModalPanelRunLoopMode` is not included either:
//! the shell runs no modals or panels.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::sync::{Arc, Mutex};

use core_foundation::base::{CFIndex, CFRelease};
use core_foundation::runloop::{
    kCFRunLoopDefaultMode, CFRunLoopAddSource, CFRunLoopGetMain, CFRunLoopSourceContext,
    CFRunLoopSourceCreate, CFRunLoopSourceRef, CFRunLoopSourceSignal, CFRunLoopWakeUp,
};

/// The run-loop source that wakes the main thread, shared by every sender.
/// Null until [`install_source`] runs on the main thread.
struct Signal(Mutex<CFRunLoopSourceRef>);

// The raw pointer is only ever used through CFRunLoopSourceSignal and
// CFRunLoopWakeUp, both thread-safe per the CF run loop contract.
unsafe impl Send for Signal {}
unsafe impl Sync for Signal {}

/// Handle for background threads to enqueue a message for the main thread.
/// Cheap to clone.
#[derive(Clone)]
pub struct MainThreadSender<E> {
    queue: Arc<Mutex<VecDeque<E>>>,
    signal: Arc<Signal>,
}

/// The queue and signal cell shared between the senders and the installed
/// drain source. Kept by the app so it can also drain synchronously: a tray
/// click applies pending messages BEFORE tray-icon shows the menu, so each
/// open presents the freshest model.
pub struct Pending<E> {
    queue: Arc<Mutex<VecDeque<E>>>,
    signal: Arc<Signal>,
}

impl<E> Clone for Pending<E> {
    fn clone(&self) -> Self {
        Pending {
            queue: Arc::clone(&self.queue),
            signal: Arc::clone(&self.signal),
        }
    }
}

/// Create the shared queue plus the sender for it. Call [`install_source`]
/// on the main thread to attach the drain (and get the sender's signal cell
/// live).
pub fn channel<E>() -> (MainThreadSender<E>, Pending<E>) {
    let queue = Arc::new(Mutex::new(VecDeque::new()));
    let signal = Arc::new(Signal(Mutex::new(std::ptr::null_mut())));
    (
        MainThreadSender {
            queue: Arc::clone(&queue),
            signal: Arc::clone(&signal),
        },
        Pending { queue, signal },
    )
}

/// Attach the drain source to the main run loop in the default run-loop mode.
/// From this point on, every sender's `send` wakes the loop. Messages queued
/// before installation are drained on the first loop pass (the source is
/// signaled right after being added).
///
/// `on_message` runs on the main thread, once per queued message, whenever
/// the loop is idle in the default mode — never during menu tracking. The
/// source and the closure live for the rest of the process (the shell has
/// exactly one queue); nothing returns them.
pub fn install_source<E, F>(pending: Pending<E>, on_message: F)
where
    E: Send + 'static,
    F: FnMut(E) + 'static,
{
    let mut on_message = on_message;
    let drain_queue = Arc::clone(&pending.queue);
    let callback: Box<dyn FnMut()> = Box::new(move || loop {
        // Pop under a short-lived guard so `on_message` never runs with the
        // lock held (its apply paths touch AppKit, not this queue — but a
        // re-entrant send must not deadlock either).
        let next = drain_queue.lock().unwrap().pop_front();
        match next {
            Some(message) => on_message(message),
            None => break,
        }
    });
    // The context's `info` owns the callback for the source's lifetime.
    let info = Box::into_raw(Box::new(callback)) as *mut c_void;

    extern "C" fn perform(info: *const c_void) {
        let callback = unsafe { &mut *(info.cast_mut() as *mut Box<dyn FnMut()>) };
        callback();
    }

    let mut context = CFRunLoopSourceContext {
        version: 0,
        info,
        retain: None,
        release: None,
        copyDescription: None,
        equal: None,
        hash: None,
        schedule: None,
        cancel: None,
        perform,
    };
    let source = unsafe { CFRunLoopSourceCreate(std::ptr::null(), CFIndex::MAX - 1, &mut context) };
    unsafe {
        // Low priority: drain behind AppKit's own event sources.
        CFRunLoopAddSource(CFRunLoopGetMain(), source, kCFRunLoopDefaultMode);
        // The run loop retains the source while attached (forever, here);
        // hand back our create-ownership.
        CFRelease(source as *const c_void as *const _);
    }
    *pending.signal.0.lock().unwrap() = source;
    // Wake the loop once so anything queued before installation drains.
    unsafe {
        CFRunLoopSourceSignal(source);
        CFRunLoopWakeUp(CFRunLoopGetMain());
    }
}

impl<E> Pending<E> {
    /// Apply every queued message via `apply` (main thread only). Returns
    /// once the queue is empty.
    pub fn drain(&self, mut apply: impl FnMut(E)) {
        loop {
            // Pop under a short-lived guard; `apply` must not re-enter, and
            // cannot deadlock on a held lock regardless.
            let next = self.queue.lock().unwrap().pop_front();
            match next {
                Some(message) => apply(message),
                None => break,
            }
        }
    }
}

impl<E> MainThreadSender<E> {
    /// Enqueue a message and wake the main run loop. The message is applied
    /// when the loop next runs its default-mode sources — immediately if the
    /// menu is closed, otherwise right after it closes.
    pub fn send(&self, message: E) {
        if let Ok(mut queue) = self.queue.lock() {
            queue.push_back(message);
        }
        let source = *self.signal.0.lock().unwrap();
        if !source.is_null() {
            unsafe {
                CFRunLoopSourceSignal(source);
                CFRunLoopWakeUp(CFRunLoopGetMain());
            }
        }
    }
}
