# Security model

What prx can and cannot do on your behalf, and where your data goes. To report a vulnerability see
[SECURITY.md](../../SECURITY.md).

- **No tokens stored by prx.** All GitHub I/O goes through the user's authenticated `gh` CLI;
  prx never sees or persists credentials.
- **LLM API keys are referenced, never stored.** An `llm_api` engine names the environment
  variable holding its key (`api_key_env`); prx reads it from its own environment at call time,
  sends it only as the request's auth header to that engine's `base_url`, and never writes it to
  config, state, history, manifests, or logs. Validation rejects a value that looks like a key
  instead of a variable name, and any inline key field. `GET /discover/agents` and `prx doctor`
  report only whether the variable is set.
- **One opt-in outbound destination.** Configuring an `llm_api` engine (as a review or brief
  engine) is the only way prx itself opens a non-local connection: HTTPS (or the `http://` you
  configured, e.g. a local server) to exactly that engine's `base_url` — the provider default when
  unset — via `reqwest` with rustls (no system OpenSSL). The review prompt with the PR diff goes
  to that endpoint (and, for a brief engine, the PR's title, description, file list, and diff), so
  choose one you trust with your code. Without an `llm_api` engine, prx's
  network footprint is unchanged.
- **No other network beyond `gh` and localhost.** The IPC API binds `127.0.0.1` only and requires the
  per-start bearer token (`daemon.token`, mode `0600`). The settings app's web UI talks only to its
  host process, which calls that same loopback API; it makes no GitHub calls and never handles
  token values (credentials are referenced by environment-variable name).
- **Prompt injection stance:** PR content is data, not instructions. The output contract that
  prx appends last to every prompt makes the read-only rule and the data-not-instructions rule
  explicit, asks the agent to report suspected injection, and prx itself performs every write.
- **What the read-only rule is, and is not.** An `agent` engine is a CLI that runs as you, with
  whatever tools and permissions you start it with (the built-in `claude` entry passes
  `--dangerously-skip-permissions`, so a background review never stops to ask). Its read-only
  rule is an instruction in the prompt; prx does not sandbox the agent. An `llm_api` engine has no
  tools: it receives the prompt and the diff and can only return text.
- **Human-only approval.** No automated path can post `--approve`; `prx approve` is the single
  approve flow, and every automated review body carries the `— automated review by prx` footer
  (or your `identity.attribution_footer`) — keep `{{IDENTITY}}` in a custom `comments.template`,
  since prx does not force it in.
- **Local data only.** `state.json` / `history.jsonl` may contain PR titles and author logins and
  stay on your disk; there is no telemetry.

See also [Architecture](architecture.md) for the design behind these rules.
