//! Single instance, reopen, and quit hooks (design A2).
//!
//! - The pid-file guard ([`crate::settings::pidfile`], `<data>/app.pid`) is
//!   claimed at start for the process lifetime. A second launch that finds a
//!   live PRX app posts the distributed notification [`OPEN_WINDOW`]
//!   (object = the page to show), activates the running instance, and exits.
//!   `LSMultipleInstancesProhibited` is not relied on: it does not cover a
//!   bare-binary dev run.
//! - The app delegate answers `applicationShouldHandleReopen:` (Finder,
//!   Spotlight, or Dock launch of the already-running bundle) by opening the
//!   window, and `applicationWillTerminate:` by running the quit hook (stop
//!   the owned daemon unless the user chose to keep it).
//! - The same delegate observes [`OPEN_WINDOW`] for the running instance.

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationOptions, NSApplicationDelegate, NSRunningApplication,
};
use objc2_foundation::{
    NSDistributedNotificationCenter, NSNotification, NSNotificationSuspensionBehavior, NSObject,
    NSObjectProtocol, NSString,
};

use crate::settings::pidfile;

/// The distributed notification a second launch posts (object: page).
pub const OPEN_WINDOW: &str = "com.prx.app.open-window";

/// What the delegate reports to the shell (main thread).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppEvent {
    /// Show the window on this page.
    OpenWindow(String),
    /// The app is terminating (any path: Quit, ⌘Q, logout).
    WillTerminate,
}

/// The outcome of the start-up instance check.
pub enum Startup {
    /// This process is the instance; keep the claim alive until exit.
    Primary(pidfile::Claim),
    /// Another instance was asked to open its window; exit now.
    Forwarded(i32),
}

/// Claim the instance, or forward `open_page` (None for a `--background`
/// launch: nothing to show) to the live instance.
pub fn claim_or_forward(open_page: Option<&str>) -> Startup {
    let path = pidfile::path();
    let own_pid = std::process::id() as i32;
    let contents = std::fs::read_to_string(&path).ok();
    match pidfile::decide(contents.as_deref(), own_pid, is_live_app) {
        pidfile::Decision::ActivateExisting(pid) => {
            if let Some(page) = open_page {
                post_open_window(page);
                if let Some(running) =
                    NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
                {
                    running.activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows);
                }
            }
            Startup::Forwarded(pid)
        }
        pidfile::Decision::Claim => Startup::Primary(pidfile::Claim::acquire(&path, own_pid)),
    }
}

/// A live process with this pid running the same executable as us (a
/// dead pid, or a reused pid now running something else, is not).
/// `NSRunningApplication` is not consulted here: before the first instance
/// has finished launching, and for launches outside LaunchServices, it does
/// not reliably know the pid.
pub fn is_live_app(pid: i32) -> bool {
    // SAFETY: kill(2) with signal 0 only probes for existence.
    if pid <= 0 || unsafe { libc::kill(pid, 0) } != 0 {
        return false;
    }
    let Some(theirs) = executable_of(pid) else {
        return false;
    };
    let ours = std::env::current_exe().ok();
    let name = |path: &std::path::Path| path.file_name().map(|n| n.to_os_string());
    ours.as_deref()
        .and_then(name)
        .is_some_and(|ours| Some(ours) == name(&theirs))
}

/// The executable path of `pid` (`proc_pidpath`).
fn executable_of(pid: i32) -> Option<std::path::PathBuf> {
    let mut buffer = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: the buffer is valid for its full length.
    let len = unsafe { libc::proc_pidpath(pid, buffer.as_mut_ptr().cast(), buffer.len() as u32) };
    if len <= 0 {
        return None;
    }
    buffer.truncate(len as usize);
    use std::os::unix::ffi::OsStringExt;
    Some(std::path::PathBuf::from(std::ffi::OsString::from_vec(
        buffer,
    )))
}

fn post_open_window(page: &str) {
    let center = NSDistributedNotificationCenter::defaultCenter();
    let name = NSString::from_str(OPEN_WINDOW);
    let object = NSString::from_str(page);
    // SAFETY: no userInfo; name/object are NSStrings, as the API requires.
    unsafe {
        center.postNotificationName_object_userInfo_deliverImmediately(
            &name,
            Some(&object),
            None,
            true,
        );
    }
}

type Handler = Box<dyn Fn(AppEvent)>;

define_class!(
    /// NSApplication delegate + distributed-notification observer.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "PrxAppDelegate"]
    #[ivars = Handler]
    pub struct AppDelegate;

    unsafe impl NSObjectProtocol for AppDelegate {}

    unsafe impl NSApplicationDelegate for AppDelegate {
        #[unsafe(method(applicationShouldHandleReopen:hasVisibleWindows:))]
        fn should_handle_reopen(&self, _sender: &NSApplication, _visible: bool) -> bool {
            (self.ivars())(AppEvent::OpenWindow("dashboard".into()));
            true
        }

        #[unsafe(method(applicationWillTerminate:))]
        fn will_terminate(&self, _notification: &NSNotification) {
            (self.ivars())(AppEvent::WillTerminate);
        }
    }

    impl AppDelegate {
        #[unsafe(method(openWindowRequested:))]
        fn open_window_requested(&self, notification: &NSNotification) {
            let page = notification
                .object()
                .and_then(|object| object.downcast::<NSString>().ok())
                .map(|page| page.to_string())
                .filter(|page| !page.is_empty())
                .unwrap_or_else(|| "dashboard".into());
            (self.ivars())(AppEvent::OpenWindow(page));
        }
    }
);

impl AppDelegate {
    /// Install as the NSApp delegate and observe [`OPEN_WINDOW`]. The
    /// returned object must stay alive for the process lifetime (NSApp
    /// holds its delegate weakly).
    pub fn install(mtm: MainThreadMarker, handler: impl Fn(AppEvent) + 'static) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Box::new(handler) as Handler);
        // SAFETY: NSObject's designated initializer.
        let delegate: Retained<Self> = unsafe { msg_send![super(this), init] };
        let app = NSApplication::sharedApplication(mtm);
        app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        let center = NSDistributedNotificationCenter::defaultCenter();
        let name = NSString::from_str(OPEN_WINDOW);
        // SAFETY: the selector exists on this class with the observer
        // signature; the delegate outlives the registration (process
        // lifetime). Deliver even while the app is inactive (an accessory
        // app usually is).
        unsafe {
            center.addObserver_selector_name_object_suspensionBehavior(
                &delegate,
                sel!(openWindowRequested:),
                Some(&name),
                None,
                NSNotificationSuspensionBehavior::DeliverImmediately,
            );
        }
        delegate
    }
}
