//! The one core browser resolver every PR-opening path uses (design D9;
//! spec prx/browser-open "Browser selection"): the menu bar's Open action,
//! notification click-through, and daemon auto-open all turn a PR URL plus
//! the effective `browser` setting into an argv here.
//!
//! No shell is involved in [`argv`]: a command template's `{{URL}}` is
//! substituted per argv element and the program is spawned directly.
//!
//! The spawn itself goes through an [`Opener`], so tests inject a spy and
//! never launch a real browser; production uses [`SystemOpener`].
//! [`open_url`] never drops the action over a bad configuration: when the
//! configured browser cannot be launched it logs a warning and falls back
//! to the system handler.

use crate::config::{AutoOpen, Browser, BROWSER_URL_PLACEHOLDER};
use crate::state::Verdict;

/// Whether a completed review with `verdict` triggers auto-open under
/// `mode` (spec prx/browser-open "Verdict-triggered auto-open"):
/// `approved` → an approved verdict (ready for the human's approval),
/// `blocking` → changes requested, `both` → either. Comment-only reviews
/// never match.
pub fn auto_open_matches(mode: AutoOpen, verdict: Verdict) -> bool {
    matches!(
        (mode, verdict),
        (AutoOpen::Approved | AutoOpen::Both, Verdict::Approved)
            | (
                AutoOpen::Blocking | AutoOpen::Both,
                Verdict::ChangesRequested
            )
    )
}

/// Spawns one browser argv (`argv[0]` is the program). Production uses
/// [`SystemOpener`]; tests inject a spy that records the argv.
pub trait Opener {
    /// Run `argv` to completion; `Err` when it cannot be spawned or exits
    /// unsuccessfully.
    fn spawn(&self, argv: &[String]) -> Result<(), String>;
}

/// The production opener: spawns the program directly (no shell) and waits
/// for it (`open`/`xdg-open` return as soon as the browser is asked).
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemOpener;

impl Opener for SystemOpener {
    fn spawn(&self, argv: &[String]) -> Result<(), String> {
        let (program, args) = argv
            .split_first()
            .ok_or_else(|| "browser command is empty".to_string())?;
        let status = std::process::Command::new(program)
            .args(args)
            .status()
            .map_err(|e| format!("cannot run {program}: {e}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("{program} exited with {status}"))
        }
    }
}

/// The platform's default-browser argv: `open <url>` on macOS,
/// `xdg-open <url>` elsewhere.
pub fn system_argv(url: &str) -> Vec<String> {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    vec![program.into(), url.into()]
}

/// The argv that opens `url` with `browser`:
/// - `system` → [`system_argv`];
/// - an app name/path → `open -a <app> <url>` on macOS (elsewhere the app
///   is run as the program: `<app> <url>`);
/// - a command template → the template with `{{URL}}` substituted in every
///   element (no shell).
pub fn argv(browser: &Browser, url: &str) -> Vec<String> {
    match browser {
        Browser::System => system_argv(url),
        Browser::App(app) if cfg!(target_os = "macos") => {
            vec!["open".into(), "-a".into(), app.clone(), url.into()]
        }
        Browser::App(app) => vec![app.clone(), url.into()],
        Browser::Command(template) => template
            .iter()
            .map(|arg| arg.replace(BROWSER_URL_PLACEHOLDER, url))
            .collect(),
    }
}

/// How [`open_url`] opened the URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenOutcome {
    /// The configured browser launched.
    Configured,
    /// The configured browser could not be launched (`reason`); the system
    /// handler opened the URL instead.
    FellBackToSystem { reason: String },
}

/// Open `url` with `browser` through `opener`. When a non-system browser
/// cannot be launched (empty template, missing app, spawn/exit failure) a
/// warning is logged and the URL is opened with the system handler instead
/// of dropping the action. `Err` only when the system handler fails too.
pub fn open_url(opener: &dyn Opener, browser: &Browser, url: &str) -> Result<OpenOutcome, String> {
    let configured = argv(browser, url);
    if matches!(browser, Browser::System) {
        return opener.spawn(&configured).map(|()| OpenOutcome::Configured);
    }
    let reason = if configured.first().is_none_or(|p| p.trim().is_empty()) {
        "browser command is empty".to_string()
    } else {
        match opener.spawn(&configured) {
            Ok(()) => return Ok(OpenOutcome::Configured),
            Err(reason) => reason,
        }
    };
    tracing::warn!(
        browser = ?browser,
        %url,
        %reason,
        "configured browser could not be launched; falling back to the system browser"
    );
    match opener.spawn(&system_argv(url)) {
        Ok(()) => Ok(OpenOutcome::FellBackToSystem { reason }),
        Err(system) => Err(format!(
            "{reason}; system browser fallback failed too: {system}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_open_matches_verdicts_per_mode() {
        use AutoOpen::*;
        use Verdict::*;
        for (mode, approved, blocking) in [
            (Off, false, false),
            (AutoOpen::Approved, true, false),
            (Blocking, false, true),
            (Both, true, true),
        ] {
            assert_eq!(
                auto_open_matches(mode, Verdict::Approved),
                approved,
                "{mode:?}"
            );
            assert_eq!(
                auto_open_matches(mode, ChangesRequested),
                blocking,
                "{mode:?}"
            );
            assert!(!auto_open_matches(mode, CommentOnly), "{mode:?}");
        }
    }
    use std::cell::RefCell;

    const URL: &str = "https://github.com/org/repo/pull/13";

    /// Records every argv; fails any argv whose program is in `failing`.
    #[derive(Default)]
    struct Spy {
        calls: RefCell<Vec<Vec<String>>>,
        failing: Vec<String>,
    }

    impl Spy {
        fn failing(programs: &[&str]) -> Self {
            Spy {
                failing: programs.iter().map(|p| p.to_string()).collect(),
                ..Spy::default()
            }
        }
        fn calls(&self) -> Vec<Vec<String>> {
            self.calls.borrow().clone()
        }
    }

    impl Opener for Spy {
        fn spawn(&self, argv: &[String]) -> Result<(), String> {
            self.calls.borrow_mut().push(argv.to_vec());
            match argv.first() {
                Some(program) if self.failing.contains(program) => {
                    Err(format!("cannot run {program}"))
                }
                _ => Ok(()),
            }
        }
    }

    fn system_program() -> &'static str {
        if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        }
    }

    #[test]
    fn system_opens_with_the_platform_default_handler() {
        assert_eq!(argv(&Browser::System, URL), [system_program(), URL]);
        assert_eq!(system_argv(URL), [system_program(), URL]);
    }

    #[test]
    fn app_opens_with_the_open_with_application_mechanism() {
        let firefox = argv(&Browser::App("Firefox".into()), URL);
        let arc = argv(&Browser::App("/Applications/Arc Browser.app".into()), URL);
        if cfg!(target_os = "macos") {
            assert_eq!(firefox, ["open", "-a", "Firefox", URL]);
            assert_eq!(arc, ["open", "-a", "/Applications/Arc Browser.app", URL]);
        } else {
            assert_eq!(firefox, ["Firefox", URL]);
            assert_eq!(arc, ["/Applications/Arc Browser.app", URL]);
        }
    }

    #[test]
    fn template_substitutes_the_url_per_element_without_a_shell() {
        let template = Browser::Command(vec![
            "firefox".into(),
            "--new-tab".into(),
            "{{URL}}".into(),
            "--ref={{URL}}".into(),
        ]);
        assert_eq!(
            argv(&template, URL),
            ["firefox", "--new-tab", URL, &format!("--ref={URL}")]
        );
        // Shell metacharacters stay inert inside one argv element.
        let hostile = "https://x/$(rm -rf ~);echo `id` 'q'";
        let out = argv(
            &Browser::Command(vec!["b".into(), "{{URL}}".into()]),
            hostile,
        );
        assert_eq!(out, ["b", hostile]);
    }

    #[test]
    fn config_forms_resolve_from_yaml_to_the_expected_argv() {
        let cases: [(&str, Vec<String>); 4] = [
            ("system", system_argv(URL)),
            ("System", system_argv(URL)),
            (
                "Firefox",
                argv(&Browser::App("Firefox".into()), URL), // platform form
            ),
            (
                "[\"arc\", \"--new\", \"{{URL}}\"]",
                vec!["arc".into(), "--new".into(), URL.into()],
            ),
        ];
        for (yaml, expected) in cases {
            let browser: Browser = serde_yaml::from_str(yaml).expect("browser parses");
            assert_eq!(argv(&browser, URL), expected, "yaml {yaml}");
        }
    }

    #[test]
    fn open_url_spawns_the_configured_argv_once() {
        let spy = Spy::default();
        let template = Browser::Command(vec!["arc".into(), "{{URL}}".into()]);
        assert_eq!(open_url(&spy, &template, URL), Ok(OpenOutcome::Configured));
        assert_eq!(spy.calls(), [vec!["arc".to_string(), URL.to_string()]]);

        let spy = Spy::default();
        assert_eq!(
            open_url(&spy, &Browser::System, URL),
            Ok(OpenOutcome::Configured)
        );
        assert_eq!(spy.calls(), [system_argv(URL)]);
    }

    #[test]
    fn unlaunchable_browser_falls_back_to_system() {
        let spy = Spy::failing(&["no-such-browser"]);
        let template = Browser::Command(vec!["no-such-browser".into(), "{{URL}}".into()]);
        let outcome = open_url(&spy, &template, URL).expect("falls back");
        assert!(matches!(outcome, OpenOutcome::FellBackToSystem { .. }));
        assert_eq!(
            spy.calls(),
            [
                vec!["no-such-browser".to_string(), URL.to_string()],
                system_argv(URL)
            ]
        );
    }

    #[test]
    fn empty_template_falls_back_without_spawning_it() {
        let spy = Spy::default();
        let outcome = open_url(&spy, &Browser::Command(vec![]), URL).expect("falls back");
        assert_eq!(
            outcome,
            OpenOutcome::FellBackToSystem {
                reason: "browser command is empty".into()
            }
        );
        assert_eq!(spy.calls(), [system_argv(URL)]);
    }

    #[test]
    fn system_failure_is_an_error_and_not_retried() {
        let spy = Spy::failing(&[system_program()]);
        assert!(open_url(&spy, &Browser::System, URL).is_err());
        assert_eq!(spy.calls().len(), 1);

        let spy = Spy::failing(&[system_program(), "nope"]);
        let err = open_url(&spy, &Browser::Command(vec!["nope".into()]), URL).unwrap_err();
        assert!(err.contains("fallback failed"), "{err}");
        assert_eq!(spy.calls().len(), 2);
    }

    /// The fallback reports why the configured browser was passed over, and
    /// when the system handler fails as well the error carries both causes.
    #[test]
    fn fallback_keeps_the_configured_browsers_failure_reason() {
        let spy = Spy::failing(&["arc"]);
        let template = Browser::Command(vec!["arc".into(), "{{URL}}".into()]);
        assert_eq!(
            open_url(&spy, &template, URL),
            Ok(OpenOutcome::FellBackToSystem {
                reason: "cannot run arc".into()
            })
        );

        let spy = Spy::failing(&["arc", system_program()]);
        assert_eq!(
            open_url(&spy, &template, URL),
            Err(format!(
                "cannot run arc; system browser fallback failed too: cannot run {}",
                system_program()
            ))
        );
    }

    /// A named application that cannot be launched (not installed) gets the
    /// same second attempt through the system handler as a command template.
    /// On macOS both attempts run `open`, so a spy failing that program
    /// fails the fallback too; elsewhere the app is its own program.
    #[test]
    fn unlaunchable_app_is_retried_with_the_system_handler() {
        let app = Browser::App("No Such Browser".into());
        let configured = argv(&app, URL);
        let spy = Spy::failing(&[configured[0].as_str()]);
        let outcome = open_url(&spy, &app, URL);
        assert_eq!(spy.calls(), [configured, system_argv(URL)]);
        if cfg!(target_os = "macos") {
            let err = outcome.unwrap_err();
            assert!(err.contains("system browser fallback failed too"), "{err}");
        } else {
            assert!(matches!(outcome, Ok(OpenOutcome::FellBackToSystem { .. })));
        }
    }

    /// A blank program is as unusable as an empty template: it is never
    /// spawned. Template elements without the placeholder pass through
    /// as written, and a URL is substituted once (text in the URL that looks
    /// like the placeholder is not expanded again).
    #[test]
    fn blank_program_is_not_spawned_and_substitution_is_single_pass() {
        let spy = Spy::default();
        let blank = Browser::Command(vec!["  ".into(), "{{URL}}".into()]);
        assert_eq!(
            open_url(&spy, &blank, URL),
            Ok(OpenOutcome::FellBackToSystem {
                reason: "browser command is empty".into()
            })
        );
        assert_eq!(spy.calls(), [system_argv(URL)]);

        let no_placeholder = Browser::Command(vec!["arc".into(), "--new-window".into()]);
        assert_eq!(argv(&no_placeholder, URL), ["arc", "--new-window"]);
        let twice = Browser::Command(vec!["b".into(), "{{URL}}#{{URL}}".into()]);
        assert_eq!(argv(&twice, "u"), ["b", "u#u"]);
        let odd_url = "https://x/?q={{URL}}";
        let out = argv(
            &Browser::Command(vec!["b".into(), "{{URL}}".into()]),
            odd_url,
        );
        assert_eq!(out, ["b", odd_url]);
        // The program position is substituted like any other element.
        let program = Browser::Command(vec!["{{URL}}".into()]);
        assert_eq!(argv(&program, "x"), ["x"]);
    }

    /// The production opener, exercised with `sh` only (never a browser): it
    /// maps the exit status to the result, names a program it cannot run,
    /// and hands every argument over verbatim, with no shell in between.
    #[test]
    fn system_opener_runs_the_argv_directly_and_reports_failures() {
        let sh = |script: &str, arg: &str| -> Vec<String> {
            ["sh", "-c", script, "sh", arg]
                .iter()
                .map(|s| s.to_string())
                .collect()
        };
        assert_eq!(SystemOpener.spawn(&sh("exit 0", "")), Ok(()));
        let err = SystemOpener.spawn(&sh("exit 3", "")).unwrap_err();
        assert!(err.starts_with("sh exited with"), "{err}");
        assert!(err.contains('3'), "{err}");

        // `$(...)`, backticks, quotes and `;` arrive as one literal argument.
        let hostile = "https://x/$(exit 7);`exit 8` 'q' \"r\" && false";
        let same = format!("[ \"$1\" = '{}' ]", hostile.replace('\'', "'\\''"));
        assert_eq!(SystemOpener.spawn(&sh(&same, hostile)), Ok(()));

        let missing = vec![
            "prx-test-no-such-browser-binary".to_string(),
            URL.to_string(),
        ];
        let err = SystemOpener.spawn(&missing).unwrap_err();
        assert!(
            err.starts_with("cannot run prx-test-no-such-browser-binary: "),
            "{err}"
        );
        assert_eq!(
            SystemOpener.spawn(&[]),
            Err("browser command is empty".to_string())
        );
    }

    /// `open_url` through the production opener with a configured command
    /// that succeeds: it runs once and nothing else is attempted. (The
    /// failing case is covered with the spy only: its fallback would open a
    /// real browser.)
    #[test]
    fn open_url_with_the_production_opener_runs_a_working_command_once() {
        let marker = std::env::temp_dir().join(format!(
            "prx-browser-opened-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_file(&marker);
        // Appends one line per launch, with the URL it was handed.
        let ok = Browser::Command(vec![
            "sh".into(),
            "-c".into(),
            "printf '%s\\n' \"$1\" >> \"$0\"".into(),
            marker.display().to_string(),
            "{{URL}}".into(),
        ]);
        assert_eq!(
            open_url(&SystemOpener, &ok, URL),
            Ok(OpenOutcome::Configured)
        );
        assert_eq!(
            std::fs::read_to_string(&marker).unwrap(),
            format!("{URL}\n")
        );
        let _ = std::fs::remove_file(&marker);
    }
}
