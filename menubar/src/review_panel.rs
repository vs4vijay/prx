//! The review panel (task 6.3, design D9) behind "Review with prompt…" and
//! the footer's "Review PR…": a floating `NSAlert` (its window is an
//! `NSPanel`) whose accessory view holds
//! - an optional single-line PR-reference field ("Review PR…" only),
//! - an engine `NSPopUpButton` (the default engine first, preselected),
//! - a "Post to GitHub" checkbox (checked),
//! - a scrollable text view for extra instructions.
//!
//! Runs app-modal on the main thread; the caller submits the result through
//! the action controller's background path, so no daemon I/O happens here.
//! The mapping from what the user chose to the request — only options that
//! differ from the defaults are sent, so a plain submit equals "Review" — is
//! pure ([`engine_choices`], [`ReviewPanelResult::into_request`]) and unit
//! tested; the AppKit layout needs manual QA.

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSApplication, NSBorderType, NSButton,
    NSControlStateValueOff, NSControlStateValueOn, NSFloatingWindowLevel, NSPopUpButton,
    NSScrollView, NSTextField, NSTextView, NSView,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use prx::review::ReviewOverrides;

/// What the panel is opened for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelSpec {
    /// The PR key for a per-PR panel; `None` shows the reference field
    /// ("Review PR…").
    pub target: Option<String>,
    /// Engine names from `/status` (empty for an older daemon).
    pub engines: Vec<String>,
    /// The PR's effective engine (`None` when unknown or for "Review PR…").
    pub default_engine: Option<String>,
}

/// One engine picker entry: its label and the override it stands for
/// (`None` = keep the repo's engine).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineChoice {
    pub label: String,
    pub engine: Option<String>,
}

/// The picker entries: the default first (preselected; sends no override),
/// then every other configured engine. With no engine list (older daemon)
/// only the default is offered.
pub fn engine_choices(engines: &[String], default: Option<&str>) -> Vec<EngineChoice> {
    let mut choices = vec![EngineChoice {
        label: match default {
            Some(engine) => format!("{engine} (default)"),
            None => "Repo default".to_string(),
        },
        engine: None,
    }];
    choices.extend(
        engines
            .iter()
            .filter(|engine| Some(engine.as_str()) != default)
            .map(|engine| EngineChoice {
                label: engine.clone(),
                engine: Some(engine.clone()),
            }),
    );
    choices
}

/// What the user submitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewPanelResult {
    /// The PR key, or the typed reference for "Review PR…".
    pub target: String,
    /// The chosen picker entry's override (`None` = the default engine).
    pub engine: Option<String>,
    /// The "Post to GitHub" checkbox.
    pub post: bool,
    /// The extra instructions (may be blank).
    pub prompt: String,
}

impl ReviewPanelResult {
    /// The review request to send: the trimmed target plus only the options
    /// that differ from the defaults. `None` when the reference is empty
    /// (the panel then does nothing).
    pub fn into_request(self) -> Option<(String, ReviewOverrides)> {
        let target = self.target.trim();
        if target.is_empty() {
            return None;
        }
        let prompt = self.prompt.trim();
        let overrides = ReviewOverrides {
            engine: self.engine.filter(|engine| !engine.trim().is_empty()),
            post: (!self.post).then_some(false),
            extra_prompt: (!prompt.is_empty()).then(|| prompt.to_string()),
        };
        Some((target.to_string(), overrides))
    }
}

/// Accessory width and control heights (points).
const WIDTH: f64 = 380.0;
const LABEL_WIDTH: f64 = 90.0;
const ROW: f64 = 26.0;
const PROMPT_HEIGHT: f64 = 110.0;
const GAP: f64 = 8.0;

/// Ask for the review options. `Some(result)` when the user pressed
/// "Review", `None` on Cancel.
pub fn ask(mtm: MainThreadMarker, spec: &PanelSpec) -> Option<ReviewPanelResult> {
    let alert = NSAlert::new(mtm);
    let (title, info) = match &spec.target {
        Some(key) => (
            format!("Review {key}"),
            "Options apply to this one run only (not saved to state or config).",
        ),
        None => (
            "Review a pull request".to_string(),
            "Enter org/repo#N, a PR URL, repo#N, or a PR number. \
             Options apply to this one run only.",
        ),
    };
    alert.setMessageText(&NSString::from_str(&title));
    alert.setInformativeText(&NSString::from_str(info));
    alert.addButtonWithTitle(&NSString::from_str("Review"));
    alert.addButtonWithTitle(&NSString::from_str("Cancel"));

    // Rows top to bottom; frames are laid out bottom-up below (AppKit's
    // origin is the bottom-left corner).
    let reference = spec.target.is_none().then(|| {
        let field = NSTextField::textFieldWithString(&NSString::from_str(""), mtm);
        field.setPlaceholderString(Some(&NSString::from_str("acme/api#42")));
        field
    });
    let choices = engine_choices(&spec.engines, spec.default_engine.as_deref());
    let popup = NSPopUpButton::initWithFrame_pullsDown(
        NSPopUpButton::alloc(mtm),
        NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(WIDTH - LABEL_WIDTH, ROW),
        ),
        false,
    );
    for choice in &choices {
        popup.addItemWithTitle(&NSString::from_str(&choice.label));
    }
    popup.selectItemAtIndex(0);
    // SAFETY: no target/action — the state is read after the modal ends.
    let post = unsafe {
        NSButton::checkboxWithTitle_target_action(
            &NSString::from_str("Post to GitHub"),
            None,
            None,
            mtm,
        )
    };
    post.setState(NSControlStateValueOn);
    let prompt_label =
        NSTextField::labelWithString(&NSString::from_str("Extra instructions (optional)"), mtm);
    let text_frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(WIDTH, PROMPT_HEIGHT));
    let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), text_frame);
    scroll.setHasVerticalScroller(true);
    scroll.setBorderType(NSBorderType::BezelBorder);
    let text: Retained<NSTextView> = NSTextView::initWithFrame(NSTextView::alloc(mtm), text_frame);
    text.setRichText(false);
    text.setVerticallyResizable(true);
    scroll.setDocumentView(Some(&text));

    let rows = usize::from(reference.is_some()) + 3;
    let height = rows as f64 * (ROW + GAP) + PROMPT_HEIGHT;
    let container = NSView::initWithFrame(
        NSView::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(WIDTH, height)),
    );
    let place = |view: &NSView, x: f64, y: f64, width: f64, h: f64| {
        view.setFrame(NSRect::new(NSPoint::new(x, y), NSSize::new(width, h)));
        container.addSubview(view);
    };
    let label = |text: &str| NSTextField::labelWithString(&NSString::from_str(text), mtm);
    let mut top = height;
    if let Some(field) = &reference {
        top -= ROW;
        place(&label("Pull request"), 0.0, top + 4.0, LABEL_WIDTH, 17.0);
        place(field, LABEL_WIDTH, top + 2.0, WIDTH - LABEL_WIDTH, 22.0);
        top -= GAP;
    }
    top -= ROW;
    place(&label("Engine"), 0.0, top + 4.0, LABEL_WIDTH, 17.0);
    place(
        &popup,
        LABEL_WIDTH - 3.0,
        top,
        WIDTH - LABEL_WIDTH + 3.0,
        ROW,
    );
    top -= GAP + ROW;
    place(&post, LABEL_WIDTH, top + 3.0, WIDTH - LABEL_WIDTH, 20.0);
    top -= GAP + ROW;
    place(&prompt_label, 0.0, top + 2.0, WIDTH, 17.0);
    place(&scroll, 0.0, 0.0, WIDTH, PROMPT_HEIGHT);

    alert.setAccessoryView(Some(&container));
    alert.layout();

    let window = alert.window();
    window.setLevel(NSFloatingWindowLevel);
    let first: &NSView = match &reference {
        Some(field) => field,
        None => &text,
    };
    window.setInitialFirstResponder(Some(first));
    // An accessory (Dock-less) app must activate itself or the panel opens
    // behind the frontmost app without keyboard focus.
    #[allow(deprecated)]
    NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
    window.makeFirstResponder(Some(first));

    if alert.runModal() != NSAlertFirstButtonReturn {
        return None;
    }
    let selected = usize::try_from(popup.indexOfSelectedItem()).unwrap_or(0);
    Some(ReviewPanelResult {
        target: match (&spec.target, &reference) {
            (Some(key), _) => key.clone(),
            (None, Some(field)) => field.stringValue().to_string(),
            (None, None) => String::new(),
        },
        engine: choices.get(selected).and_then(|c| c.engine.clone()),
        post: post.state() != NSControlStateValueOff,
        prompt: text.string().to_string(),
    })
}

/// Ask before a GitHub write (Approve, Block): a floating alert with the
/// confirming button first and Cancel (Esc) second. `true` to proceed.
pub fn confirm(mtm: MainThreadMarker, request: &crate::actions::Confirmation) -> bool {
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(&request.title));
    alert.setInformativeText(&NSString::from_str(&request.body));
    alert.addButtonWithTitle(&NSString::from_str(&request.button));
    alert.addButtonWithTitle(&NSString::from_str("Cancel"));
    alert.window().setLevel(NSFloatingWindowLevel);
    // An accessory (Dock-less) app must activate itself or the alert opens
    // behind the frontmost app without keyboard focus.
    #[allow(deprecated)]
    NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
    alert.runModal() == NSAlertFirstButtonReturn
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_default_engine_leads_and_sends_no_override() {
        let choices = engine_choices(&names(&["claude", "codex", "pi"]), Some("codex"));
        let labels: Vec<&str> = choices.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, ["codex (default)", "claude", "pi"]);
        assert_eq!(choices[0].engine, None);
        assert_eq!(choices[1].engine.as_deref(), Some("claude"));
    }

    #[test]
    fn older_daemons_offer_only_the_default() {
        let choices = engine_choices(&[], Some("claude"));
        assert_eq!(
            choices,
            [EngineChoice {
                label: "claude (default)".into(),
                engine: None
            }]
        );
        let choices = engine_choices(&[], None);
        assert_eq!(choices.len(), 1);
        assert_eq!(choices[0].label, "Repo default");
        // "Review PR…" (no known default) lists every engine after it.
        assert_eq!(engine_choices(&names(&["claude", "codex"]), None).len(), 3);
    }

    fn result(target: &str, engine: Option<&str>, post: bool, prompt: &str) -> ReviewPanelResult {
        ReviewPanelResult {
            target: target.into(),
            engine: engine.map(str::to_string),
            post,
            prompt: prompt.into(),
        }
    }

    #[test]
    fn a_plain_submit_is_a_plain_review() {
        let (target, overrides) = result(" acme/api#42 ", None, true, "  \n ")
            .into_request()
            .unwrap();
        assert_eq!(target, "acme/api#42");
        assert_eq!(overrides, ReviewOverrides::default());
    }

    #[test]
    fn every_changed_option_is_sent() {
        let (target, overrides) = result("42", Some("codex"), false, " check auth \n")
            .into_request()
            .unwrap();
        assert_eq!(target, "42");
        assert_eq!(
            overrides,
            ReviewOverrides {
                engine: Some("codex".into()),
                post: Some(false),
                extra_prompt: Some("check auth".into()),
            }
        );
        // Post stays unset when checked (the repo's mode decides).
        let (_, overrides) = result("42", None, true, "x").into_request().unwrap();
        assert_eq!(overrides.post, None);
    }

    #[test]
    fn an_empty_reference_does_nothing() {
        assert_eq!(result("  ", Some("codex"), false, "x").into_request(), None);
    }
}
