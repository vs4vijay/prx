# Environment variables

## Read by prx

| Variable | Effect |
|----------|--------|
| `PRX_CONFIG` | Path of the config file (default `~/.config/prx/config.yml`). |
| `PRX_DATA_DIR` | Data directory for `state.json`, `history.jsonl`, `daemon.*`, `app.pid`, `logs/`, `manifests/` (default `~/.local/share/prx`). |
| `VISUAL`, `EDITOR` | The editor `prx config edit` opens (`VISUAL` first). |

`PRX_CONFIG` and `PRX_DATA_DIR` are primarily for tests, but they also let you run multiple isolated prx homes side by side.

## Named by your config

prx never stores secrets. Config keys hold the **name** of a variable, and prx reads the value
from the daemon's environment at call time:

| Config key | The named variable holds |
|------------|--------------------------|
| `engines.<name>.api_key_env` | The API key of an `llm_api` [engine](../guides/engines.md). |
| `defaults.identity.credential` (also per org / repo) | A GitHub token, passed to `gh` as `GH_TOKEN` for every GitHub write on that repo — see [Browser, identity, and Block](../guides/browser-identity-block.md). |

A daemon started at login by launchd or systemd does not read your shell profile, so a variable
exported only in `~/.zshrc` is invisible to it. See [Autostart](../guides/autostart.md) and
[Troubleshooting](../troubleshooting.md).

## Set by prx for the agent

| Variable | Value |
|----------|-------|
| `PRX_MANIFEST_PATH` | The file the agent must write its [review manifest](review-manifest.md) to (also in the prompt as `{{MANIFEST_PATH}}`). |
| `PRX_PR_NUMBER` | The PR number under review. |
| `PRX_REPO` | The repository under review, as `org/repo`. |
