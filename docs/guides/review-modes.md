# Review modes

`review_mode` is the review posture, settable in `defaults` and per org / per repo:

| Mode | Polling + state | AI reviews | GitHub writes | Notifications |
|------|-----------------|------------|---------------|---------------|
| `review comment` *(default)* | yes | yes | review body + inline comments | yes |
| `monitor` | yes | yes — verdicts stay in prx (state, history, menu) | **none** | yes |
| `off` | **no** — no `gh` polling; last-known state keeps serving | none, forced or not | none | none |

`off` is a stop switch, not a shutdown: the daemon stays up and answers IPC, and the status payload
reports `review_mode: "off"` so the menu bar shows *Reviews off* (and the pause-bars glyph). The
human actions (Approve, Block, Unblock) are never gated by the mode, and no mode ever approves a
PR. A mode is a lasting posture; to hold reviews for a while, [pause](pausing.md) instead.
Switch the global mode from the menu bar's *Review mode: …* submenu, the app window's *General*
page (three cards, each saying whether reviews run, what is posted to GitHub, and where verdicts
show), or the CLI:

```bash
prx config set defaults.review_mode monitor
prx config set defaults.review_mode "review comment"
```
