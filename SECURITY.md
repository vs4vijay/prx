# Security policy

## Reporting a vulnerability

Please report security problems privately, not in a public issue or pull request.

Use GitHub's private vulnerability reporting for this repository:

1. Open the [Security tab](https://github.com/vs4vijay/prx/security) of the repository.
2. Choose **Report a vulnerability**
   ([direct link](https://github.com/vs4vijay/prx/security/advisories/new)).
3. Describe the problem, the version or commit you tested, and the steps to reproduce it.

You will get a reply in the advisory thread. Please give the maintainer reasonable time to fix
the problem before disclosing it publicly.

## What is in scope

prx is built around a few security properties. A way to break any of them is a vulnerability:

- A review engine, or content inside a pull request, causing prx to approve or merge a pull
  request, or to post anything other than the validated review.
- Pull request content being executed, or being treated as instructions by prx itself.
- A credential (a GitHub token or an LLM API key) being written to the config, state, history,
  manifests, or logs.
- The localhost API being reachable without the bearer token, or from another host.
- prx opening a network connection other than through `gh`, to localhost, or to an LLM API
  endpoint you configured.

The design behind these properties is described in the
[security model](docs/concepts/security.md).

## Supported versions

Security fixes are made on the `main` branch and included in the next release. Older releases are
not patched.

## Not a vulnerability

- A coding agent ignoring its read-only instruction and writing to GitHub with its own
  credentials. prx tells the agent not to, and never gives it prx's write path, but it cannot
  sandbox a third-party CLI. Run agents you trust, with the permissions you intend.
- PRX.app being ad-hoc signed and not notarized. This is a known limitation of the current
  releases.
