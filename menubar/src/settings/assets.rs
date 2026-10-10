//! The embedded settings UI (`menubar/settings-ui/`), served to the webview
//! over the `prx://` custom protocol so pages can reference sibling files
//! (`app.css`, `bridge.js`, …) without a build step or any network access.
//! To add a file: drop it in `settings-ui/` and add a row to [`ASSETS`].

/// The custom URL scheme and the page the window opens on. WKWebView
/// serves custom schemes as `<scheme>://<host>/<path>`.
pub const SCHEME: &str = "prx";
pub const START_URL: &str = "prx://localhost/index.html";

/// `(path, content type, bytes)` for every embedded file.
const ASSETS: &[(&str, &str, &[u8])] = &[
    (
        "index.html",
        "text/html; charset=utf-8",
        include_bytes!("../../settings-ui/index.html"),
    ),
    (
        "app.css",
        "text/css; charset=utf-8",
        include_bytes!("../../settings-ui/app.css"),
    ),
    (
        "bridge.js",
        "text/javascript; charset=utf-8",
        include_bytes!("../../settings-ui/bridge.js"),
    ),
    (
        "icons.js",
        "text/javascript; charset=utf-8",
        include_bytes!("../../settings-ui/icons.js"),
    ),
    (
        "fields.js",
        "text/javascript; charset=utf-8",
        include_bytes!("../../settings-ui/fields.js"),
    ),
    (
        "cron.js",
        "text/javascript; charset=utf-8",
        include_bytes!("../../settings-ui/cron.js"),
    ),
    (
        "schema.js",
        "text/javascript; charset=utf-8",
        include_bytes!("../../settings-ui/schema.js"),
    ),
    (
        "form.js",
        "text/javascript; charset=utf-8",
        include_bytes!("../../settings-ui/form.js"),
    ),
    (
        "app-section.js",
        "text/javascript; charset=utf-8",
        include_bytes!("../../settings-ui/app-section.js"),
    ),
    (
        "pages-defaults.js",
        "text/javascript; charset=utf-8",
        include_bytes!("../../settings-ui/pages-defaults.js"),
    ),
    (
        "pages-messages.js",
        "text/javascript; charset=utf-8",
        include_bytes!("../../settings-ui/pages-messages.js"),
    ),
    (
        "pages-repos.js",
        "text/javascript; charset=utf-8",
        include_bytes!("../../settings-ui/pages-repos.js"),
    ),
    (
        "pages-misc.js",
        "text/javascript; charset=utf-8",
        include_bytes!("../../settings-ui/pages-misc.js"),
    ),
    (
        "overview.js",
        "text/javascript; charset=utf-8",
        include_bytes!("../../settings-ui/overview.js"),
    ),
    (
        "brief-card.js",
        "text/javascript; charset=utf-8",
        include_bytes!("../../settings-ui/brief-card.js"),
    ),
    (
        "chart.js",
        "text/javascript; charset=utf-8",
        include_bytes!("../../settings-ui/chart.js"),
    ),
    (
        "pages-dashboard.js",
        "text/javascript; charset=utf-8",
        include_bytes!("../../settings-ui/pages-dashboard.js"),
    ),
    (
        "pages-prs.js",
        "text/javascript; charset=utf-8",
        include_bytes!("../../settings-ui/pages-prs.js"),
    ),
    (
        "pages-history.js",
        "text/javascript; charset=utf-8",
        include_bytes!("../../settings-ui/pages-history.js"),
    ),
    (
        "pages-usage.js",
        "text/javascript; charset=utf-8",
        include_bytes!("../../settings-ui/pages-usage.js"),
    ),
    (
        "app.js",
        "text/javascript; charset=utf-8",
        include_bytes!("../../settings-ui/app.js"),
    ),
];

/// Look up an embedded asset by URL path (`/`, `/index.html`, `/app.js`).
pub fn lookup(path: &str) -> Option<(&'static str, &'static [u8])> {
    let path = path.trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    ASSETS
        .iter()
        .find(|(name, _, _)| *name == path)
        .map(|(_, content_type, bytes)| (*content_type, *bytes))
}

/// Navigation policy: the webview may only ever show embedded pages.
pub fn is_allowed_navigation(url: &str) -> bool {
    url.starts_with("prx://localhost/") || url == "about:blank"
}

/// A link the window hands to the user's browser instead of navigating
/// (design D14: "Open in GitHub" on the Pull requests page). Only `https`
/// web links qualify; everything else stays blocked.
pub fn is_external_link(url: &str) -> bool {
    url.starts_with("https://")
}

/// A window page id as the app accepts it (`dashboard`, `general`, …);
/// anything else falls back to Dashboard so a bad id never reaches JS.
fn page_id(page: &str) -> &str {
    let valid = !page.is_empty()
        && page
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if valid {
        page
    } else {
        "dashboard"
    }
}

/// The URL a new window opens on: the app reads `#page=<id>` at start.
pub fn start_url(page: &str) -> String {
    format!("{START_URL}#page={}", page_id(page))
}

/// The script that switches an already-open window to `page`.
pub fn go_script(page: &str) -> String {
    format!("window.prxApp && window.prxApp.go(\"{}\");", page_id(page))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_embedded_page_resolves() {
        assert_eq!(lookup("/").unwrap().0, "text/html; charset=utf-8");
        assert!(lookup("/index.html").is_some());
        assert!(lookup("/bridge.js")
            .unwrap()
            .0
            .starts_with("text/javascript"));
        assert!(lookup("/app.css").is_some());
        assert!(lookup("/app.js").is_some());
        assert!(lookup("/../Cargo.toml").is_none());
        assert!(lookup("/missing.js").is_none());
    }

    #[test]
    fn index_references_only_embedded_files_and_forbids_network() {
        let (_, html) = lookup("/index.html").unwrap();
        let html = std::str::from_utf8(html).unwrap();
        for file in ["app.css", "bridge.js", "app.js"] {
            assert!(html.contains(file), "{file}");
            assert!(lookup(file).is_some(), "{file}");
        }
        assert!(html.contains("connect-src 'none'"));
    }

    fn ui_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("settings-ui")
    }

    fn asset_text(name: &str) -> &'static str {
        std::str::from_utf8(
            lookup(name)
                .unwrap_or_else(|| panic!("{name} not embedded"))
                .1,
        )
        .unwrap()
    }

    #[test]
    fn every_file_in_settings_ui_is_embedded_and_loaded() {
        let html = asset_text("index.html");
        let files: Vec<String> = std::fs::read_dir(ui_dir())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|name| !name.starts_with('.'))
            .collect();
        for name in &files {
            assert!(
                lookup(name).is_some(),
                "settings-ui/{name} is missing from ASSETS"
            );
            if name.ends_with(".js") || name.ends_with(".css") {
                assert!(
                    html.contains(&format!("\"{name}\"")),
                    "index.html never loads {name}"
                );
            }
        }
        assert_eq!(
            ASSETS.len(),
            files.len(),
            "ASSETS lists a file not in settings-ui/"
        );
    }

    /// The form's field table (schema.js) must cover every overridable
    /// config field (the same paths the daemon reports sources for) and
    /// every `defaults` key, so nothing in src/config.rs is unreachable.
    #[test]
    fn schema_covers_every_config_field() {
        let schema = asset_text("schema.js");
        let has_key = |path: &str| schema.contains(&format!("key: \"{path}\""));
        let sources =
            prx::config::Config::default().effective_sources(&prx::config::RepoConfig::default());
        assert!(!sources.is_empty());
        for path in sources.keys() {
            let event = path.strip_prefix("notifications.");
            let covered =
                has_key(path) || event.is_some_and(|e| schema.contains(&format!("[\"{e}\",")));
            assert!(covered, "schema.js has no field for `{path}`");
        }
        let defaults = serde_json::to_value(prx::config::Defaults::default()).unwrap();
        for key in defaults.as_object().unwrap().keys() {
            assert!(
                has_key(key)
                    || schema.contains(&format!("key: \"{key}."))
                    || sources.contains_key(key)
                    || sources.keys().any(|p| p.starts_with(&format!("{key}."))),
                "schema.js has no field for defaults.{key}"
            );
        }
        // `schedule` is skipped when unset, so check it explicitly.
        assert!(has_key("schedule"));
    }

    #[test]
    fn schema_lists_the_builtin_engines() {
        let schema = asset_text("schema.js");
        let line = schema
            .lines()
            .find(|l| l.contains("const BUILTIN_ENGINES"))
            .expect("BUILTIN_ENGINES");
        for name in prx::config::builtin_engines().keys() {
            assert!(line.contains(&format!("\"{name}\"")), "{name}");
        }
    }

    /// The pure JS helpers have node unit tests (settings-ui-tests/); run
    /// them when node is installed, skip quietly otherwise.
    #[test]
    fn js_unit_tests_pass_when_node_is_available() {
        let tests = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("settings-ui-tests");
        let files: Vec<_> = std::fs::read_dir(&tests)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.to_string_lossy().ends_with(".test.js"))
            .collect();
        assert!(!files.is_empty(), "no JS tests in {}", tests.display());
        let Ok(output) = std::process::Command::new("node")
            .arg("--test")
            .args(&files)
            .output()
        else {
            eprintln!("node not found: skipping settings-ui JS tests");
            return;
        };
        assert!(
            output.status.success(),
            "node --test failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn window_pages_open_by_url_or_script() {
        assert_eq!(
            start_url("dashboard"),
            "prx://localhost/index.html#page=dashboard"
        );
        assert_eq!(
            start_url("general"),
            "prx://localhost/index.html#page=general"
        );
        assert_eq!(start_url("x\");alert(1)//"), start_url("dashboard"));
        assert_eq!(
            go_script("general"),
            "window.prxApp && window.prxApp.go(\"general\");"
        );
        assert!(go_script("</script>").contains("\"dashboard\""));
        // The app knows both entry pages and the hash/go entry points.
        let schema = asset_text("schema.js");
        for page in ["\"dashboard\"", "\"general\""] {
            assert!(schema.contains(page), "{page}");
        }
        let app = asset_text("app.js");
        assert!(app.contains("pageFromHash") && app.contains("go: show"));
    }

    #[test]
    fn external_links_are_https_only() {
        assert!(is_external_link("https://github.com/acme/api/pull/42"));
        assert!(!is_external_link("http://127.0.0.1:7777/config"));
        assert!(!is_external_link("file:///etc/passwd"));
        assert!(!is_external_link("prx://localhost/index.html"));
    }

    #[test]
    fn navigation_stays_on_embedded_pages() {
        assert!(is_allowed_navigation(START_URL));
        assert!(is_allowed_navigation(&start_url("history")));
        assert!(!is_allowed_navigation("https://github.com/"));
        assert!(!is_allowed_navigation("http://127.0.0.1:7777/config"));
        assert!(!is_allowed_navigation("file:///etc/passwd"));
    }
}
