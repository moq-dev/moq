# [S] Backport outbound HTTP trust to release

## Goal

On `release`, outbound relay HTTP clients cannot be steered by a network
attacker: auth verifies its server independently from mesh TLS, and the cluster
API refuses redirects and remote plaintext HTTP.

## Plan

The main fix is [moq-dev/moq#7199](https://github.com/moq-dev/moq/pull/7199).
It adds `--auth-root` / `auth.root`, shares only the mesh client identity,
refuses cluster API redirects including redirect response bodies, and permits
plaintext API URLs only on loopback.

After that PR lands, backport these fixes in a separate PR against `release`,
as `CONTRIBUTING.md` requires. Preserve the published Rust API and released
cluster config spellings. Unlike main, release's cluster HTTP client also
inherits mesh server trust; isolate it too, retaining the client identity and
support for a private API CA. Build trust before background polling starts so
malformed roots fail startup.

The original 2026-10-27 audit findings (AUTH-1, RELAY-2) were verified at
[8d2fa6653a](https://github.com/moq-dev/moq/commit/8d2fa6653a). Release has the
same auth coupling (`config.auth.init(node, &config.connect.tls)`) and no
cluster API redirect policy.

Public API: additive auth and cluster API trust options, with the existing
release method signatures preserved. Wire: none.
