//! PRX.app's own notification sender (design A7, spec app-bundle
//! "Notifications from PRX").
//!
//! When the shell runs from its bundle, banners go through
//! `UNUserNotificationCenter`, so macOS names "PRX" (with the app icon) as
//! the sender and the user controls PRX under System Settings →
//! Notifications. Only the delivery mechanism changes: which events banner,
//! the coalescing, and the daemon's deferral to a live app (D3) stay as they
//! are, so each event still has exactly one sender.
//!
//! Route per delivery tick ([`route`]):
//! - permission prompt not answered yet → fallback (macOS can report
//!   `denied` while its prompt is still on screen);
//! - authorized (or provisional/ephemeral) → this center;
//! - denied by the user → no banner at all (the choice is respected; no
//!   other notifier is used);
//! - not determined (prompt unanswered, or macOS rejected the bundle, e.g.
//!   an ad-hoc bundle outside an Applications folder), no bundle (bare dev
//!   binary), or the status query timed out → the previous
//!   terminal-notifier/osascript path in [`crate::banners`].
//!
//! `currentNotificationCenter` raises an Objective-C exception in a process
//! without a bundle, so nothing here touches it unless [`init`] found a
//! bundle identifier.

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use block2::{DynBlock, RcBlock};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{define_class, msg_send, AllocAnyThread};
use objc2_foundation::{NSBundle, NSDictionary, NSError, NSObject, NSObjectProtocol, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNAuthorizationStatus, UNMutableNotificationContent, UNNotification,
    UNNotificationPresentationOptions, UNNotificationRequest, UNNotificationResponse,
    UNNotificationSettings, UNNotificationSound, UNUserNotificationCenter,
    UNUserNotificationCenterDelegate,
};

use crate::banners::Banner;

/// How long a delivery tick waits for `getNotificationSettings` before
/// falling back (the poller thread must never hang on the center).
const SETTINGS_TIMEOUT: Duration = Duration::from_secs(2);

/// `userInfo` key carrying the PR's web URL for click-through.
const URL_KEY: &str = "url";

/// Set once by [`init`]: the process runs from a bundle, so the center may
/// be used.
static AVAILABLE: AtomicBool = AtomicBool::new(false);

/// Set when the permission request completed (granted, declined, or
/// failed). Until then the prompt is unanswered, and macOS may already
/// report the status as `denied` while its prompt is on screen, so the
/// status alone cannot tell "pending" from "declined".
static ANSWERED: AtomicBool = AtomicBool::new(false);

/// Last raw `authorizationStatus` seen (logged on change); -1 = none yet.
static LAST_STATUS: AtomicIsize = AtomicIsize::new(-1);

/// Disambiguates identifiers of banners raised in the same nanosecond.
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// The user's notification permission for PRX, as the center reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Authorization {
    /// No answer yet, or macOS will not let this bundle ask.
    NotDetermined,
    Denied,
    /// Authorized, provisional, or ephemeral: banners may be posted.
    Allowed,
}

/// Where one tick's banners go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// PRX.app posts them through `UNUserNotificationCenter`.
    Center,
    /// The user turned PRX notifications off: show nothing.
    Drop,
    /// The previous terminal-notifier/osascript/notify-send path.
    Fallback,
}

/// The routing rule of design A7. `answered` is whether the permission
/// request has completed; `authorization` is `None` when the status could
/// not be read in time.
pub fn route(in_bundle: bool, answered: bool, authorization: Option<Authorization>) -> Route {
    if !in_bundle || !answered {
        return Route::Fallback;
    }
    match authorization {
        Some(Authorization::Allowed) => Route::Center,
        Some(Authorization::Denied) => Route::Drop,
        Some(Authorization::NotDetermined) | None => Route::Fallback,
    }
}

fn authorization_of(status: UNAuthorizationStatus) -> Authorization {
    match status {
        UNAuthorizationStatus::Denied => Authorization::Denied,
        UNAuthorizationStatus::Authorized
        | UNAuthorizationStatus::Provisional
        | UNAuthorizationStatus::Ephemeral => Authorization::Allowed,
        _ => Authorization::NotDetermined,
    }
}

/// One notification request, fully decided as plain data (unit-tested).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub identifier: String,
    pub title: String,
    pub body: String,
    /// Tier sound name (`Basso`, …), when the tier sets one.
    pub sound: Option<String>,
    /// Groups a PR's banners together in Notification Center.
    pub thread: String,
    /// The PR's web URL for click-through.
    pub url: Option<String>,
}

/// The request for `banner`; `stamp` makes the identifier unique.
pub fn notice(banner: &Banner, stamp: u128) -> Notice {
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let thread = if banner.key.is_empty() {
        "prx".to_string()
    } else {
        banner.key.clone()
    };
    Notice {
        identifier: format!("prx-{stamp}-{sequence}"),
        title: banner.title.clone(),
        body: banner.body.clone(),
        sound: banner.sound.clone(),
        thread,
        url: banner.url.clone(),
    }
}

/// The URL a clicked banner should open: only web URLs (a `userInfo` value
/// is never handed to `open` unless it is http(s)).
pub fn click_url(value: Option<String>) -> Option<String> {
    value.filter(|url| url.starts_with("https://") || url.starts_with("http://"))
}

define_class!(
    /// `UNUserNotificationCenterDelegate`: show banners while the PRX window
    /// is frontmost, and open the PR when a banner is clicked. The center
    /// may call it on any thread.
    #[unsafe(super(NSObject))]
    #[name = "PrxNotificationCenterDelegate"]
    pub struct CenterDelegate;

    unsafe impl NSObjectProtocol for CenterDelegate {}

    unsafe impl UNUserNotificationCenterDelegate for CenterDelegate {
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn will_present(
            &self,
            _center: &UNUserNotificationCenter,
            _notification: &UNNotification,
            completion_handler: &DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            completion_handler.call((UNNotificationPresentationOptions::Banner
                | UNNotificationPresentationOptions::List
                | UNNotificationPresentationOptions::Sound,));
        }

        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn did_receive(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            completion_handler: &DynBlock<dyn Fn()>,
        ) {
            let user_info = response.notification().request().content().userInfo();
            let key = NSString::from_str(URL_KEY);
            let value = user_info
                .objectForKey(AsRef::<AnyObject>::as_ref(&*key))
                .and_then(|object| object.downcast::<NSString>().ok())
                .map(|url| url.to_string());
            match click_url(value) {
                Some(url) => {
                    eprintln!("prx-menubar: banner clicked — opening {url}");
                    // Resolves the repo's browser through the daemon on a
                    // background thread (same path as app-window links).
                    crate::browser::open_external(url);
                }
                None => eprintln!("prx-menubar: banner clicked (no PR link)"),
            }
            completion_handler.call(());
        }
    }
);

/// Set up the center when running from a bundle: install the delegate and
/// ask for permission (alert + sound). Call on the main thread before
/// `NSApplication::run`, so a click that launched the app is delivered.
/// Keep the returned delegate alive for the process lifetime (the center
/// holds it weakly). `None` outside a bundle (fallback path only).
pub fn init() -> Option<Retained<CenterDelegate>> {
    let bundle_id = NSBundle::mainBundle().bundleIdentifier()?;
    AVAILABLE.store(true, Ordering::SeqCst);
    let center = UNUserNotificationCenter::currentNotificationCenter();
    // SAFETY: NSObject's designated initializer on a fresh allocation.
    let delegate: Retained<CenterDelegate> =
        unsafe { msg_send![super(CenterDelegate::alloc().set_ivars(())), init] };
    center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    let handler = RcBlock::new(move |granted: objc2::runtime::Bool, error: *mut NSError| {
        ANSWERED.store(true, Ordering::SeqCst);
        // SAFETY: the center passes a valid NSError or nil.
        match unsafe { error.as_ref() } {
            Some(error) => eprintln!(
                "prx-menubar: notification permission unavailable ({}) — banners use \
                 terminal-notifier/osascript",
                error.localizedDescription()
            ),
            None if granted.as_bool() => {
                eprintln!("prx-menubar: notifications allowed — banners are sent as PRX")
            }
            None => eprintln!(
                "prx-menubar: notifications are off for PRX (System Settings → \
                 Notifications → PRX) — no banners will be shown"
            ),
        }
    });
    center.requestAuthorizationWithOptions_completionHandler(
        UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
        &handler,
    );
    eprintln!("prx-menubar: notification center ready for {bundle_id}");
    Some(delegate)
}

/// Read the current permission (bounded wait). `None` outside a bundle or
/// when the center did not answer within [`SETTINGS_TIMEOUT`].
pub fn authorization() -> Option<Authorization> {
    if !AVAILABLE.load(Ordering::SeqCst) {
        return None;
    }
    let (tx, rx) = mpsc::sync_channel::<Authorization>(1);
    let handler = RcBlock::new(move |settings: std::ptr::NonNull<UNNotificationSettings>| {
        // SAFETY: the center passes a valid settings object.
        let status = unsafe { settings.as_ref() }.authorizationStatus();
        if LAST_STATUS.swap(status.0, Ordering::SeqCst) != status.0 {
            eprintln!(
                "prx-menubar: notification permission status = {} ({:?})",
                status.0,
                authorization_of(status)
            );
        }
        let _ = tx.try_send(authorization_of(status));
    });
    UNUserNotificationCenter::currentNotificationCenter()
        .getNotificationSettingsWithCompletionHandler(&handler);
    rx.recv_timeout(SETTINGS_TIMEOUT).ok()
}

/// This tick's route (queries the permission only from a bundle).
pub fn current_route() -> Route {
    let in_bundle = AVAILABLE.load(Ordering::SeqCst);
    let answered = ANSWERED.load(Ordering::SeqCst);
    let authorization = if in_bundle && answered {
        authorization()
    } else {
        None
    };
    route(in_bundle, answered, authorization)
}

/// Post one banner through the center. Delivery errors are logged by the
/// completion handler (best-effort, like every notifier).
pub fn post(banner: &Banner) {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    let notice = notice(banner, stamp);
    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str(&notice.title));
    content.setBody(&NSString::from_str(&notice.body));
    content.setThreadIdentifier(&NSString::from_str(&notice.thread));
    if let Some(sound) = &notice.sound {
        content.setSound(Some(&UNNotificationSound::soundNamed(&NSString::from_str(
            sound,
        ))));
    }
    if let Some(url) = &notice.url {
        let key = NSString::from_str(URL_KEY);
        let value = NSString::from_str(url);
        let info: Retained<NSDictionary<NSString, NSString>> =
            NSDictionary::from_slices(&[&*key], &[&*value]);
        // SAFETY: an NSDictionary of NSString → NSString is a valid
        // property-list userInfo; the generic parameters are erased at
        // runtime, so viewing it as NSDictionary<AnyObject, AnyObject> is
        // sound.
        unsafe {
            let erased: &NSDictionary = &*(Retained::as_ptr(&info) as *const NSDictionary);
            content.setUserInfo(erased);
        }
    }
    let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
        &NSString::from_str(&notice.identifier),
        &content,
        None,
    );
    let summary = format!("{} — {}", notice.title, notice.body);
    let handler = RcBlock::new(move |error: *mut NSError| {
        // SAFETY: the center passes a valid NSError or nil.
        if let Some(error) = unsafe { error.as_ref() } {
            eprintln!(
                "prx-menubar: banner delivery failed ({summary}): {}",
                error.localizedDescription()
            );
        }
    });
    UNUserNotificationCenter::currentNotificationCenter()
        .addNotificationRequest_withCompletionHandler(&request, Some(&handler));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn banner(key: &str, url: Option<&str>, sound: Option<&str>) -> Banner {
        Banner {
            key: key.into(),
            title: format!("PRX · {key}"),
            body: "review: 2 blocking issue(s) found".into(),
            sound: sound.map(Into::into),
            url: url.map(Into::into),
        }
    }

    #[test]
    fn route_uses_the_center_only_from_a_bundle_with_permission() {
        assert_eq!(
            route(true, true, Some(Authorization::Allowed)),
            Route::Center
        );
        assert_eq!(
            route(false, true, Some(Authorization::Allowed)),
            Route::Fallback
        );
    }

    #[test]
    fn denied_permission_drops_instead_of_falling_back() {
        assert_eq!(route(true, true, Some(Authorization::Denied)), Route::Drop);
    }

    #[test]
    fn an_unanswered_prompt_falls_back_even_if_the_status_reads_denied() {
        // macOS reports `denied` while its permission prompt is on screen.
        assert_eq!(
            route(true, false, Some(Authorization::Denied)),
            Route::Fallback
        );
        assert_eq!(route(true, false, None), Route::Fallback);
    }

    #[test]
    fn unanswered_unavailable_or_unknown_permission_falls_back() {
        assert_eq!(
            route(true, true, Some(Authorization::NotDetermined)),
            Route::Fallback
        );
        assert_eq!(route(true, true, None), Route::Fallback);
        assert_eq!(route(false, false, None), Route::Fallback);
    }

    #[test]
    fn authorization_status_mapping() {
        assert_eq!(
            authorization_of(UNAuthorizationStatus::Authorized),
            Authorization::Allowed
        );
        assert_eq!(
            authorization_of(UNAuthorizationStatus::Provisional),
            Authorization::Allowed
        );
        assert_eq!(
            authorization_of(UNAuthorizationStatus::Ephemeral),
            Authorization::Allowed
        );
        assert_eq!(
            authorization_of(UNAuthorizationStatus::Denied),
            Authorization::Denied
        );
        assert_eq!(
            authorization_of(UNAuthorizationStatus::NotDetermined),
            Authorization::NotDetermined
        );
    }

    #[test]
    fn notice_carries_title_body_sound_thread_and_url() {
        let url = "https://github.com/org/repo/pull/13";
        let notice = notice(&banner("org/repo#13", Some(url), Some("Basso")), 42);
        assert_eq!(notice.title, "PRX · org/repo#13");
        assert_eq!(notice.body, "review: 2 blocking issue(s) found");
        assert_eq!(notice.sound.as_deref(), Some("Basso"));
        assert_eq!(notice.thread, "org/repo#13");
        assert_eq!(notice.url.as_deref(), Some(url));
        assert!(notice.identifier.starts_with("prx-42-"));
    }

    #[test]
    fn aggregate_banner_groups_under_prx_without_a_link() {
        let notice = notice(&banner("", None, None), 7);
        assert_eq!(notice.thread, "prx");
        assert_eq!(notice.url, None);
        assert_eq!(notice.sound, None);
    }

    #[test]
    fn identifiers_are_unique_within_one_stamp() {
        let b = banner("org/repo#1", None, None);
        assert_ne!(notice(&b, 1).identifier, notice(&b, 1).identifier);
    }

    #[test]
    fn click_opens_only_web_urls() {
        let url = "https://github.com/org/repo/pull/13".to_string();
        assert_eq!(click_url(Some(url.clone())), Some(url));
        assert_eq!(click_url(Some("file:///etc/passwd".into())), None);
        assert_eq!(click_url(Some("/Applications/Calculator.app".into())), None);
        assert_eq!(click_url(None), None);
    }
}
