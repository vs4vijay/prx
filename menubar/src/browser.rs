//! Menu bar side of the browser resolver (design D9; spec prx/browser-open
//! "Browser selection"). The argv building and the open itself live in core
//! [`prx::browser`]; this module keeps the shell-specific pieces: resolving
//! a repo's effective `browser` from the daemon's `GET /config` effective
//! view ([`for_repo`]/[`resolve`]), [`repo_of`], and [`shell_command`].
//!
//! The effective setting is fetched at the moment a PR is opened or a
//! click-through banner is raised (no cache to go stale: config edits apply
//! to the next open). Any failure to fetch falls back to the system
//! handler, so opening a PR never fails over config.
//!
//! terminal-notifier's `-execute` runs through `/bin/sh -c`, so
//! [`shell_command`] single-quotes every element of the core argv for that
//! path.

use prx::browser::{OpenOutcome, SystemOpener};
use prx::config::Browser;
use prx::ipc::{DaemonClient, EffectiveConfig};

/// The argv that opens a URL with a browser — the core resolver
/// ([`prx::browser::argv`]), re-exported for the banner click-through.
pub use prx::browser::argv;

/// The repo's effective browser from the daemon's effective view: the
/// repo's own entry, else its org's entry, else the defaults (a repo no
/// longer configured still opens with its org/default setting).
pub fn for_repo(effective: &EffectiveConfig, repo: &str) -> Browser {
    if let Some(entry) = effective.repos.iter().find(|r| r.settings.name == repo) {
        return entry.settings.browser.clone();
    }
    let org = repo.split_once('/').map(|(org, _)| org).unwrap_or("");
    if let Some(entry) = effective
        .orgs
        .iter()
        .find(|o| !org.is_empty() && o.settings.name == org)
    {
        return entry.settings.browser.clone();
    }
    effective.defaults.browser.clone()
}

/// The repo part of a PR key (`org/repo#13` → `org/repo`).
pub fn repo_of(key: &str) -> &str {
    key.split_once('#').map(|(repo, _)| repo).unwrap_or(key)
}

/// Fetch the effective view and resolve `repo`'s browser; the system
/// handler when the daemon is unreachable or the request fails.
pub async fn resolve(client: Option<&DaemonClient>, repo: &str) -> Browser {
    let Some(client) = client else {
        return Browser::System;
    };
    match client.get_config().await {
        Ok(config) => for_repo(&config.effective, repo),
        Err(error) => {
            eprintln!(
                "prx-menubar: GET /config failed ({error:#}) — opening with the system browser"
            );
            Browser::System
        }
    }
}

/// Open `url` with `browser` through the core opener (spawned directly, no
/// shell). An unlaunchable configured browser falls back to the system
/// handler (core logs the warning; it is echoed here since the shell has no
/// tracing subscriber).
pub fn open(browser: &Browser, url: &str) -> Result<(), String> {
    match prx::browser::open_url(&SystemOpener, browser, url)? {
        OpenOutcome::Configured => {}
        OpenOutcome::FellBackToSystem { reason } => eprintln!(
            "prx-menubar: configured browser could not be launched ({reason}) — \
             opened with the system browser"
        ),
    }
    Ok(())
}

/// The `org/repo` of a GitHub web URL (`https://github.com/org/repo/pull/13`
/// → `org/repo`), used to pick the repo's browser for links opened from the
/// app window. `None` for any other URL.
pub fn repo_of_url(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://github.com/")?;
    let mut parts = rest.split(['/', '?', '#']);
    let (org, repo) = (parts.next()?, parts.next()?);
    (!org.is_empty() && !repo.is_empty()).then(|| format!("{org}/{repo}"))
}

/// Open an external link from the app window (an "Open in GitHub" link)
/// with the repo's effective browser, on a background thread so the GUI
/// never waits on the daemon. Failures are logged.
pub fn open_external(url: String) {
    let spawned = std::thread::Builder::new()
        .name("prx-open-link".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    eprintln!("prx-menubar: cannot open {url}: {error}");
                    return;
                }
            };
            let browser = runtime.block_on(async {
                match repo_of_url(&url) {
                    Some(repo) => {
                        let client = DaemonClient::discover().await;
                        resolve(client.as_ref(), &repo).await
                    }
                    None => Browser::System,
                }
            });
            match open(&browser, &url) {
                Ok(()) => eprintln!("prx-menubar: opened {url}"),
                Err(error) => eprintln!("prx-menubar: cannot open {url}: {error}"),
            }
        });
    if let Err(error) = spawned {
        eprintln!("prx-menubar: cannot start link thread: {error}");
    }
}

/// `argv` as one POSIX shell command with every element single-quoted, for
/// terminal-notifier's `-execute` (which runs via `/bin/sh -c`): nothing in
/// the URL or the template can expand or split.
pub fn shell_command(argv: &[String]) -> String {
    argv.iter()
        .map(|arg| format!("'{}'", arg.replace('\'', r"'\''")))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_of_url_reads_github_links_only() {
        assert_eq!(
            repo_of_url("https://github.com/acme/api/pull/42").as_deref(),
            Some("acme/api")
        );
        assert_eq!(
            repo_of_url("https://github.com/acme/api#x").as_deref(),
            Some("acme/api")
        );
        assert_eq!(repo_of_url("https://github.com/acme"), None);
        assert_eq!(repo_of_url("https://example.com/acme/api"), None);
    }

    #[test]
    fn shell_command_single_quotes_every_element() {
        let line = shell_command(&["open".into(), "-a".into(), "it's".into(), "a b;$(x)".into()]);
        assert_eq!(line, r"'open' '-a' 'it'\''s' 'a b;$(x)'");
    }

    fn effective(
        defaults: Browser,
        orgs: &[(&str, Browser)],
        repos: &[(&str, Browser)],
    ) -> EffectiveConfig {
        let config = prx::config::Config::default();
        let base = config.effective_defaults().expect("defaults resolve");
        let with = |name: &str, browser: Browser| prx::config::EffectiveWithSources {
            settings: prx::config::EffectiveRepo {
                name: name.to_string(),
                browser,
                ..base.clone()
            },
            sources: Default::default(),
        };
        EffectiveConfig {
            defaults: prx::config::EffectiveRepo {
                browser: defaults,
                ..base.clone()
            },
            orgs: orgs.iter().map(|(n, b)| with(n, b.clone())).collect(),
            repos: repos.iter().map(|(n, b)| with(n, b.clone())).collect(),
        }
    }

    #[test]
    fn repo_beats_org_beats_defaults() {
        let view = effective(
            Browser::App("Safari".into()),
            &[("acme", Browser::App("Firefox".into()))],
            &[(
                "acme/web",
                Browser::Command(vec!["arc".into(), "{{URL}}".into()]),
            )],
        );
        assert_eq!(
            for_repo(&view, "acme/web"),
            Browser::Command(vec!["arc".into(), "{{URL}}".into()])
        );
        assert_eq!(for_repo(&view, "acme/api"), Browser::App("Firefox".into()));
        assert_eq!(for_repo(&view, "other/x"), Browser::App("Safari".into()));
        assert_eq!(repo_of("acme/web#13"), "acme/web");
    }
}
