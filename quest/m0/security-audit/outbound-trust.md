# [S] Outbound HTTP clients verify their peer

## Goal

On `main` and on `release`, the relay's two outbound HTTP clients cannot be
steered by a network attacker:

- The auth client (`--auth-url`) verifies its server with its own trust
  settings, so `--connect-tls-insecure`, fingerprints, or roots meant for the
  cluster mesh never apply to it.
- The cluster API client (`--cluster-connect-api`) refuses redirects and
  refuses `http://` except to loopback, so an injected peer list cannot
  collect the shared cluster token.

## Plan

From the 2026-10-27 audit (AUTH-1, RELAY-2), verified at
[8d2fa6653a](https://github.com/moq-dev/moq/commit/8d2fa6653a):

- `rs/moq-relay/src/relay.rs` passes `config.connect.tls` to
  `config.auth.init`, and the auth client builds its TLS from it. With
  `--connect-tls-insecure` (documented for a self-signed mesh), a MITM on an
  `https://` auth server can answer `{"publish":["**"],"subscribe":["**"]}`
  and every client is admitted with full access.
- `rs/moq-relay/src/http_client.rs` keeps reqwest's default redirect policy,
  and `connect_api_is_http` accepts any `http://`. The cluster token is then
  appended to every peer the API lists.

Fix by copying the patterns that already exist: the cluster API client's own
server trust (`api_tls` in `http_client.rs`) for the auth client, sharing
only the client identity; and moq-auth's client policy
([moq-dev/moq#6384](https://github.com/moq-dev/moq/pull/6384): no redirects,
loopback-only plaintext) for the cluster API client. Update
`doc/bin/relay/auth.md` and the cluster docs for the new trust option.

Backport: `release` has the same coupling (`config.auth.init(node,
&config.connect.tls)`) and a cluster API client with no redirect policy;
backport in a separate PR against `release`.

Public API: a new auth trust option in relay config and CLI. Wire: none.
