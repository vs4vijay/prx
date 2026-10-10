//! Browser picker bridge methods (spec browser-picker, design W1). App-local
//! like [`super::app_bridge`]: only this UI process links AppKit, so the
//! daemon is never involved and the picker works while it is offline.
//!
//! ```text
//! JS → Rust   {"id": 1, "method": "browsers.list",   "params": {"paths": ["/Applications/Zen.app"]}}
//!             {"id": 2, "method": "browsers.choose", "params": {}}
//!             {"id": 3, "method": "browsers.test",   "params": {"browser": "Firefox"}}
//! ```
//!
//! - `browsers.list` answers `{browsers: [Entry…], extra: [Entry…]}`. Each
//!   entry is `{name, bundle_id, path, icon_png_base64, is_default}`.
//!   `browsers` is every app LaunchServices offers for `https` URLs
//!   (`NSWorkspace URLsForApplicationsToOpenURL:`), deduplicated by bundle
//!   ID, sorted by name with the system default
//!   (`URLForApplicationToOpenURL:`) first. `extra` resolves the optional
//!   `paths` (app paths the config already names that may not be https
//!   handlers, e.g. one picked with "Other app…") when they still exist.
//! - `browsers.choose` shows an `NSOpenPanel` limited to applications,
//!   starting in `/Applications`, and answers the chosen app's entry, or
//!   `null` when cancelled. AppKit panels are main-thread only, so the
//!   window host runs it (see [`super::bridge::Bridge::with_chooser`]).
//! - `browsers.test` opens [`TEST_URL`] with a `browser` value through core
//!   [`prx::browser`] without the system fallback, so a broken choice fails
//!   inline (the error names the program or app). Nothing is saved.

use std::path::Path;

use prx::browser::{Opener, SystemOpener};
use prx::config::{Browser, BROWSER_URL_PLACEHOLDER};
use serde::Serialize;
use serde_json::{json, Value};

use super::bridge::{BridgeError, Response};

/// The harmless page a "Test" opens: the prx project.
pub const TEST_URL: &str = "https://github.com/vs4vijay/prx";

/// Icon edge in pixels (design W1: 32px PNGs).
const ICON_PX: isize = 32;

/// One installed app that can open `https` URLs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Entry {
    /// The app's file name without `.app` (what `open -a` resolves).
    pub name: String,
    pub bundle_id: String,
    pub path: String,
    /// 32px PNG, base64; `None` when AppKit gave no icon.
    pub icon_png_base64: Option<String>,
    pub is_default: bool,
}

/// Does this JS message name a `browsers.*` method?
pub fn is_browser_message(body: &str) -> bool {
    method_of(body).is_some_and(|method| method.starts_with("browsers."))
}

/// The `method` of a JS message, when readable.
pub fn method_of(body: &str) -> Option<String> {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.get("method").and_then(Value::as_str).map(str::to_string))
}

/// The `id` of a JS message (`null` when unreadable).
pub fn id_of(body: &str) -> Value {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.get("id").cloned())
        .unwrap_or(Value::Null)
}

/// Handle `browsers.list` / `browsers.test` (blocking; off the main
/// thread). `browsers.choose` is answered by the window host instead.
pub fn handle(body: &str) -> Response {
    let message: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let id = message.get("id").cloned().unwrap_or(Value::Null);
    let method = message.get("method").and_then(Value::as_str).unwrap_or("");
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    eprintln!("prx-menubar: settings bridge {method} (id {id})");
    let result = match method {
        "browsers.list" => Ok(list(&params)),
        "browsers.test" => match params.get("browser") {
            Some(value) => match serde_json::from_value::<Browser>(value.clone()) {
                Ok(browser) => {
                    test(&SystemOpener, &browser).map(|()| json!({ "opened": TEST_URL }))
                }
                Err(_) => Err(error(
                    "bad_request",
                    "browsers.test: `browser` must be a string or a list of strings",
                )),
            },
            None => Err(error("bad_request", "browsers.test: `browser` is required")),
        },
        "browsers.choose" => Err(error(
            "unsupported",
            "choosing an app needs the settings window",
        )),
        other => Err(error(
            "bad_request",
            format!("unknown bridge method '{other}'"),
        )),
    };
    match result {
        Ok(value) => Response::success(id, value),
        Err(error) => Response::failure(id, error),
    }
}

fn error(kind: &str, message: impl Into<String>) -> BridgeError {
    BridgeError {
        kind: kind.into(),
        message: message.into(),
        field: None,
    }
}

fn list(params: &Value) -> Value {
    let (candidates, default_bundle) = discover();
    let browsers = dedup_sort(candidates, default_bundle.as_deref());
    let extra: Vec<Entry> = params
        .get("paths")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|path| !browsers.iter().any(|b| same_path(&b.path, path)))
        .filter_map(entry_for_path)
        .collect();
    json!({ "browsers": browsers, "extra": extra })
}

fn same_path(a: &str, b: &str) -> bool {
    a.trim_end_matches('/') == b.trim_end_matches('/')
}

/// Deduplicate by bundle ID (LaunchServices lists every copy of an app; the
/// first one, its preferred copy, is kept, except that the default's own
/// path wins for the default), mark the default, and sort by name
/// (case-insensitive) with the default first.
pub fn dedup_sort(candidates: Vec<Entry>, default_bundle: Option<&str>) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    for mut entry in candidates {
        let key = dedup_key(&entry);
        entry.is_default = default_bundle.is_some_and(|d| d.eq_ignore_ascii_case(&entry.bundle_id));
        match out.iter_mut().find(|e| dedup_key(e) == key) {
            Some(existing) => {
                if entry.is_default && !existing.is_default {
                    *existing = entry;
                }
            }
            None => out.push(entry),
        }
    }
    out.sort_by(|a, b| {
        b.is_default
            .cmp(&a.is_default)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.path.cmp(&b.path))
    });
    out
}

/// Apps without a bundle ID dedupe by path.
fn dedup_key(entry: &Entry) -> String {
    if entry.bundle_id.is_empty() {
        format!("path:{}", entry.path)
    } else {
        entry.bundle_id.to_lowercase()
    }
}

/// Open [`TEST_URL`] with `browser` — no system fallback, so a broken choice
/// is reported. The error names the program (command) or app.
pub fn test(opener: &dyn Opener, browser: &Browser) -> Result<(), BridgeError> {
    match browser {
        Browser::App(app) if app.trim().is_empty() => {
            return Err(error("browser", "choose an app to test"))
        }
        Browser::Command(argv) => {
            let count: usize = argv
                .iter()
                .map(|a| a.matches(BROWSER_URL_PLACEHOLDER).count())
                .sum();
            if argv.first().is_none_or(|p| p.trim().is_empty()) {
                return Err(error("browser", "the command is empty"));
            }
            if count != 1 {
                return Err(error(
                    "browser",
                    format!("the command must contain {BROWSER_URL_PLACEHOLDER} exactly once"),
                ));
            }
        }
        _ => {}
    }
    let argv = prx::browser::argv(browser, TEST_URL);
    opener.spawn(&argv).map_err(|reason| {
        let message = match browser {
            Browser::App(app) => format!("{app} could not be opened ({reason})"),
            _ => reason,
        };
        error("browser", message)
    })
}

/// The display name `open -a` resolves: the bundle's file name without
/// `.app`.
pub fn app_name(path: &str) -> String {
    let file = Path::new(path.trim_end_matches('/'))
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();
    file.strip_suffix(".app")
        .map(str::to_string)
        .unwrap_or(file)
}

// ------------------------------------------------------------- AppKit

#[cfg(target_os = "macos")]
fn discover() -> (Vec<Entry>, Option<String>) {
    use objc2_app_kit::NSWorkspace;
    use objc2_foundation::{NSString, NSURL};

    let workspace = NSWorkspace::sharedWorkspace();
    let Some(probe) = NSURL::URLWithString(&NSString::from_str(TEST_URL)) else {
        return (Vec::new(), None);
    };
    let default_bundle = workspace
        .URLForApplicationToOpenURL(&probe)
        .and_then(|url| url.path())
        .and_then(|path| bundle_id(&path.to_string()));
    let entries = workspace
        .URLsForApplicationsToOpenURL(&probe)
        .iter()
        .filter_map(|url| url.path())
        .filter_map(|path| entry_for_path(&path.to_string()))
        .collect();
    (entries, default_bundle)
}

#[cfg(not(target_os = "macos"))]
fn discover() -> (Vec<Entry>, Option<String>) {
    (Vec::new(), None)
}

#[cfg(target_os = "macos")]
fn bundle_id(path: &str) -> Option<String> {
    use objc2_foundation::{NSBundle, NSString};
    NSBundle::bundleWithPath(&NSString::from_str(path))
        .and_then(|bundle| bundle.bundleIdentifier())
        .map(|id| id.to_string())
}

/// The entry for an app at `path`; `None` when it is not an app bundle.
#[cfg(target_os = "macos")]
pub fn entry_for_path(path: &str) -> Option<Entry> {
    let path = path.trim_end_matches('/');
    if !path.ends_with(".app") || !Path::new(path).is_dir() {
        return None;
    }
    Some(Entry {
        name: app_name(path),
        bundle_id: bundle_id(path).unwrap_or_default(),
        path: path.to_string(),
        icon_png_base64: icon_png(path).map(|png| {
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD.encode(png)
        }),
        is_default: false,
    })
}

#[cfg(not(target_os = "macos"))]
pub fn entry_for_path(_path: &str) -> Option<Entry> {
    None
}

/// The app's Finder icon as a 32x32 PNG (drawn into an offscreen bitmap;
/// AppKit image drawing is fine off the main thread with a per-thread
/// context).
#[cfg(target_os = "macos")]
fn icon_png(path: &str) -> Option<Vec<u8>> {
    use objc2::AnyThread;
    use objc2_app_kit::{
        NSBitmapFormat, NSBitmapImageFileType, NSBitmapImageRep, NSCompositingOperation,
        NSDeviceRGBColorSpace, NSGraphicsContext, NSWorkspace,
    };
    use objc2_foundation::{NSDictionary, NSPoint, NSRect, NSSize, NSString};

    let image = NSWorkspace::sharedWorkspace().iconForFile(&NSString::from_str(path));
    // SAFETY: null planes ask AppKit to allocate its own 32x32 RGBA buffer.
    let rep = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bitmapFormat_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            std::ptr::null_mut(),
            ICON_PX,
            ICON_PX,
            8,
            4,
            true,
            false,
            NSDeviceRGBColorSpace,
            NSBitmapFormat::empty(),
            0,
            0,
        )
    }?;
    let side = ICON_PX as f64;
    rep.setSize(NSSize::new(side, side));
    let context = NSGraphicsContext::graphicsContextWithBitmapImageRep(&rep)?;
    NSGraphicsContext::saveGraphicsState_class();
    NSGraphicsContext::setCurrentContext(Some(&context));
    image.drawInRect_fromRect_operation_fraction(
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(side, side)),
        NSRect::ZERO,
        NSCompositingOperation::Copy,
        1.0,
    );
    context.flushGraphics();
    NSGraphicsContext::restoreGraphicsState_class();
    // SAFETY: an empty properties dictionary is valid for PNG.
    let data = unsafe {
        rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new())
    }?;
    Some(data.to_vec())
}

/// `browsers.choose`: an `NSOpenPanel` limited to applications, starting in
/// `/Applications`. `Ok(Value::Null)` when cancelled. Main thread only.
#[cfg(target_os = "macos")]
pub fn choose(mtm: objc2::MainThreadMarker) -> Result<Value, BridgeError> {
    use objc2_app_kit::{NSModalResponseOK, NSOpenPanel};
    use objc2_foundation::{NSArray, NSString, NSURL};

    let panel = NSOpenPanel::openPanel(mtm);
    panel.setCanChooseFiles(true);
    panel.setCanChooseDirectories(false);
    panel.setAllowsMultipleSelection(false);
    panel.setTitle(Some(&NSString::from_str("Choose an app to open PRs with")));
    panel.setPrompt(Some(&NSString::from_str("Choose")));
    // `allowedContentTypes` needs UniformTypeIdentifiers (not linked); the
    // extension filter is equivalent for app bundles.
    #[allow(deprecated)]
    panel.setAllowedFileTypes(Some(&NSArray::from_retained_slice(&[NSString::from_str(
        "app",
    )])));
    let applications = NSURL::fileURLWithPath(&NSString::from_str("/Applications"));
    panel.setDirectoryURL(Some(&applications));
    if panel.runModal() != NSModalResponseOK {
        return Ok(Value::Null);
    }
    let Some(path) = panel.URL().and_then(|url| url.path()) else {
        return Ok(Value::Null);
    };
    let path = path.to_string();
    let mut entry = entry_for_path(&path)
        .ok_or_else(|| error("browser", format!("{path} is not an application")))?;
    // The chosen app may be the system default browser.
    let (_, default_bundle) = discover();
    entry.is_default = default_bundle
        .as_deref()
        .is_some_and(|d| !entry.bundle_id.is_empty() && d.eq_ignore_ascii_case(&entry.bundle_id));
    serde_json::to_value(entry).map_err(|e| error("browser", e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn entry(name: &str, bundle_id: &str, path: &str) -> Entry {
        Entry {
            name: name.into(),
            bundle_id: bundle_id.into(),
            path: path.into(),
            icon_png_base64: None,
            is_default: false,
        }
    }

    #[test]
    fn dedups_by_bundle_and_sorts_with_the_default_first() {
        let list = dedup_sort(
            vec![
                entry("Safari", "com.apple.Safari", "/Applications/Safari.app"),
                entry(
                    "firefox",
                    "org.mozilla.firefox",
                    "/Applications/Firefox.app",
                ),
                entry(
                    "Arc",
                    "company.thebrowser.Browser",
                    "/Users/me/Applications/Arc.app",
                ),
                entry("Arc", "company.thebrowser.Browser", "/Applications/Arc.app"),
                entry(
                    "Safari",
                    "com.apple.Safari",
                    "/System/Cryptexes/App/System/Applications/Safari.app",
                ),
            ],
            Some("company.thebrowser.browser"),
        );
        let names: Vec<_> = list
            .iter()
            .map(|e| (e.name.as_str(), e.is_default))
            .collect();
        assert_eq!(
            names,
            vec![("Arc", true), ("firefox", false), ("Safari", false)]
        );
        // The first (preferred) copy of a duplicate is kept.
        assert_eq!(list[0].path, "/Users/me/Applications/Arc.app");
        assert_eq!(list[2].path, "/Applications/Safari.app");
    }

    #[test]
    fn the_default_copy_wins_and_bundleless_apps_dedupe_by_path() {
        let mut later = entry("Zen", "app.zen", "/Applications/Zen.app");
        later.icon_png_base64 = Some("x".into());
        let list = dedup_sort(
            vec![
                entry("Tool", "", "/opt/Tool.app"),
                entry("Tool", "", "/opt/Tool.app"),
                entry("Tool", "", "/usr/local/Tool.app"),
                later,
            ],
            None,
        );
        assert_eq!(list.len(), 3);
        assert!(list.iter().all(|e| !e.is_default));
        assert_eq!(list[0].path, "/opt/Tool.app");
        assert_eq!(list[1].path, "/usr/local/Tool.app");
        assert_eq!(list[2].name, "Zen");
        // No default: plain name order.
        assert!(dedup_sort(Vec::new(), Some("x")).is_empty());
    }

    #[test]
    fn app_names_drop_the_extension() {
        assert_eq!(app_name("/Applications/Google Chrome.app"), "Google Chrome");
        assert_eq!(app_name("/Applications/Zen.app/"), "Zen");
    }

    struct Spy {
        argv: RefCell<Vec<Vec<String>>>,
        fail: Option<String>,
    }

    impl Opener for Spy {
        fn spawn(&self, argv: &[String]) -> Result<(), String> {
            self.argv.borrow_mut().push(argv.to_vec());
            match &self.fail {
                Some(reason) => Err(reason.clone()),
                None => Ok(()),
            }
        }
    }

    fn spy(fail: Option<&str>) -> Spy {
        Spy {
            argv: RefCell::new(Vec::new()),
            fail: fail.map(str::to_string),
        }
    }

    #[test]
    fn test_opens_the_project_page_without_falling_back() {
        let ok = spy(None);
        test(
            &ok,
            &Browser::Command(vec!["firefox".into(), "{{URL}}".into()]),
        )
        .unwrap();
        assert_eq!(
            ok.argv.borrow()[0],
            vec!["firefox".to_string(), TEST_URL.to_string()]
        );

        let broken = spy(Some("cannot run nosuchbrowser: No such file or directory"));
        let err = test(
            &broken,
            &Browser::Command(vec!["nosuchbrowser".into(), "{{URL}}".into()]),
        )
        .unwrap_err();
        assert!(err.message.contains("nosuchbrowser"), "{}", err.message);
        // One attempt only: the system handler is never tried.
        assert_eq!(broken.argv.borrow().len(), 1);

        let app = spy(Some("open exited with exit status: 1"));
        let err = test(&app, &Browser::App("Brave".into())).unwrap_err();
        assert!(
            err.message.starts_with("Brave could not be opened"),
            "{}",
            err.message
        );
    }

    #[test]
    fn test_rejects_templates_without_exactly_one_url() {
        let never = spy(None);
        for argv in [
            vec!["firefox".to_string()],
            vec!["f".into(), "{{URL}}".into(), "{{URL}}".into()],
        ] {
            let err = test(&never, &Browser::Command(argv)).unwrap_err();
            assert!(err.message.contains("exactly once"), "{}", err.message);
        }
        assert!(never.argv.borrow().is_empty());
    }

    #[test]
    fn only_browser_methods_are_routed_here() {
        assert!(is_browser_message(
            r#"{"id":1,"method":"browsers.list","params":{}}"#
        ));
        assert!(!is_browser_message(
            r#"{"id":1,"method":"app.info","params":{}}"#
        ));
        assert!(!is_browser_message("garbage"));
        assert_eq!(id_of(r#"{"id":7,"method":"browsers.choose"}"#), json!(7));
    }

    /// Manual check (task 5.1): `cargo test -p prx-menubar -- --ignored
    /// --nocapture lists_installed_browsers` prints this machine's browsers.
    #[test]
    #[ignore]
    #[cfg(target_os = "macos")]
    fn lists_installed_browsers() {
        let value = list(&json!({}));
        for b in value["browsers"].as_array().unwrap() {
            println!(
                "{}{}  {}  {}  icon={}B",
                b["name"].as_str().unwrap(),
                if b["is_default"].as_bool().unwrap() {
                    " (default)"
                } else {
                    ""
                },
                b["bundle_id"].as_str().unwrap(),
                b["path"].as_str().unwrap(),
                b["icon_png_base64"].as_str().map_or(0, str::len),
            );
            if let Ok(dir) = std::env::var("PRX_ICON_DUMP") {
                use base64::Engine as _;
                let png = base64::engine::general_purpose::STANDARD
                    .decode(b["icon_png_base64"].as_str().unwrap_or(""))
                    .unwrap();
                std::fs::write(format!("{dir}/{}.png", b["name"].as_str().unwrap()), png).unwrap();
            }
        }
    }
}
