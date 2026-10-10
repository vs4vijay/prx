// The "PRX app" section of Settings → General (tasks 6.6/6.7): the "Open at
// login" switch and "Install command-line tool…". Both are app-local bridge
// calls (menubar/src/settings/app_bridge.rs) that never touch the daemon or
// the config draft, so they apply immediately and need no Save.
(function () {
  "use strict";
  const UI = window.prxForm;

  function bridge(method, params) {
    return window.prxBridge.call(method, params || {});
  }

  function cliStatus(cli) {
    switch (cli && cli.state) {
      case "linked": return "Installed: `" + cli.link + "` runs this app's `prx`.";
      case "linked_elsewhere": return "`" + cli.link + "` points to another prx. Installing repoints it to this app.";
      case "blocked": return "`" + cli.link + "` is a file PRX did not create, so PRX will not replace it.";
      case "missing": return "Adds `" + (cli.link || "~/.local/bin/prx") + "` so `prx` works in Terminal.";
      default: return "Not available in this build.";
    }
  }

  // Build the section; it fills itself in once `app.info` answers.
  function appSection(app) {
    const toggle = UI.el("input", { type: "checkbox", role: "switch", class: "switch", id: "app-login-item" });
    const loginNote = UI.el("p", { class: "help", role: "status" });
    const loginError = UI.el("p", { class: "field-error", role: "alert", hidden: true });
    const install = UI.button("Install command-line tool…", null);
    const cliNote = UI.el("p", { class: "help", role: "status" });
    const cliError = UI.el("p", { class: "field-error", role: "alert", hidden: true });
    toggle.disabled = true;
    install.disabled = true;

    function showError(node, error) {
      node.hidden = !error;
      node.textContent = error ? (error.message || String(error)) : "";
    }

    function render(info) {
      const login = info.login_item || {};
      toggle.checked = !!login.enabled;
      toggle.disabled = !info.bundle && !login.enabled;
      loginNote.replaceChildren.apply(loginNote, UI.rich(
        !info.bundle ? "Available when PRX runs from PRX.app (`scripts/bundle-macos.sh`)."
          : login.enabled && !login.this_app ? "Registered for another copy: `" + login.app + "`. Turn off and on to use this one."
            : "Starts PRX in the menu bar when you log in. The window stays closed."));
      const cli = info.cli || {};
      install.disabled = cli.state === "unavailable" || cli.state === "blocked" || cli.state === "linked";
      install.textContent = cli.state === "linked" ? "Installed" : "Install command-line tool…";
      cliNote.replaceChildren.apply(cliNote, UI.rich(cliStatus(cli) + (info.warning ? " " + info.warning : "")));
    }

    function refresh() {
      return bridge("app.info").then(render, function (error) { showError(loginError, error); });
    }

    toggle.addEventListener("change", function () {
      const wanted = toggle.checked;
      toggle.disabled = true;
      showError(loginError, null);
      bridge("app.login_item.set", { enabled: wanted }).then(render, function (error) {
        toggle.checked = !wanted;
        toggle.disabled = false;
        showError(loginError, error);
      });
    });
    install.addEventListener("click", function () {
      install.disabled = true;
      showError(cliError, null);
      bridge("app.cli.install").then(render, function (error) {
        install.disabled = false;
        showError(cliError, error);
      });
    });

    // The row label ("Open at login", a <label for>) names the switch.
    const loginControl = UI.el("div", { class: "inline" }, [toggle, loginError]);
    const cliControl = UI.el("div", {}, [install, cliError]);
    const rows = [
      UI.row(app, "app.login_item", "Open at login", { node: loginControl, focus: toggle }, { help: "" }),
      UI.row(app, "app.cli", "Command-line tool", { node: cliControl, focus: install }, { help: "" }),
    ];
    // The help slots hold live status text.
    rows[0].querySelector && rows[0].querySelector(".row-head").append(loginNote);
    rows[1].querySelector && rows[1].querySelector(".row-head").append(cliNote);
    refresh();
    return UI.section("PRX app", rows, "Applies right away; no Save needed.");
  }

  window.prxAppSection = appSection;
})();
