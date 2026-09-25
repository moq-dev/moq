# [M] Automatic ACME certificates

## Goal

`moq-relay` provisions and renews its own publicly trusted certificate from
an ACME directory (Let's Encrypt by default) when an `[acme]` section is
configured, so running a relay no longer needs certbot or a hand-copied
certificate. The certificate persists on disk, so a restart serves at once;
only a missing, expiring, or no-longer-matching certificate performs a
challenge, and renewal happens in the background without a restart.

## Plan

Use [rustls-acme](https://crates.io/crates/rustls-acme) rather than building
the client, cache, and renewal loop by hand: it provides the ACME account and
order flow, a directory cache for the account key and certificate, renewal
ahead of expiry, and a `ResolvesServerCert` that answers TLS-ALPN-01
challenges on the same acceptor that serves traffic. Check the crate builds
on the workspace's aws-lc-rs provider with no second provider in the tree;
`deny.toml` gates the dependency.

- **Config.** `[acme]` with `domains`, `contact`, `dir` (required; packaging
  already grants `/var/lib/moq-relay`), and `directory` (Let's Encrypt
  production by default, staging documented). Setting it alongside
  `listen.tls.cert`/`key` is a config error; a wildcard in `domains` is a
  config error since TLS-ALPN-01 cannot issue one.
- **Challenge.** TLS-ALPN-01 only. The ACME server connects to port 443 of
  every domain; the relay's `web.https` acceptor must be bound there (or
  forwarded), and config validation requires it. HTTP-01 is not offered: it
  needs port 80, and the crate's resolver already answers the challenge
  where the relay already listens.
- **Delivery.** The crate's resolver becomes a certificate source for the
  shared served-identity handle from
  [One served identity](/quest/m1/2924-moq-relay-tls-rotation-is-not-atomic-across-thread-per.md),
  so QUIC, HTTPS, and every worker on both runtimes serve the same
  certificate and pick up a renewal on their next handshake. Keys are
  written owner-only (`0600`) and `acme.dir` is created `0700`.
- **Startup.** With no usable cached certificate, bind `web.https` first
  and serve only the challenge until issuance completes; QUIC, the other
  routes, and readiness wait for it, since a self-signed placeholder cannot
  serve browsers. The first issuance is bounded by a startup budget (capped
  backoff with jitter, a few minutes, configurable) and exits with the ACME
  error, the domain, and the port on failure. A cached certificate that is
  expired, does not cover every configured domain, or was issued by a
  different directory counts as missing.
- **Docs.** `doc/bin/relay/config.md` gains the section; `doc/setup/prod.md`
  makes ACME the recommended path and keeps the external-certificate path;
  the packaging TOML and the nix module expose `acme.dir` under the state
  directory.

Tests: an in-process ACME test server (or Pebble behind a feature flag)
issues on first run over the HTTPS acceptor; a restart with a valid cache
performs no challenge; a cache missing a domain or from another directory
reissues; an unreachable directory fails the first start inside the budget
with an actionable error; a renewal reaches every worker through a fresh
handshake without restart; a failed renewal leaves the old certificate
serving; the config rejects `[acme]` alongside explicit paths, without
`web.https`, or with a wildcard.

## Required

- [One served identity](/quest/m1/2924-moq-relay-tls-rotation-is-not-atomic-across-thread-per.md) - every listener must share one reloadable identity before ACME can promise renewal

## Closes

- [#709](https://github.com/moq-dev/moq/issues/709) - close this issue when the quest finishes
