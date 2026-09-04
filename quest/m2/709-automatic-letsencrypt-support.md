# [L] Automatic ACME certificates

## Goal

`moq-relay` provisions and renews its own publicly trusted certificate from
an ACME directory (Let's Encrypt by default) when an `[acme]` section is
configured, so running a relay no longer needs certbot or a hand-copied
certificate. The certificate persists on disk, so a restart serves at once
and only a missing, expiring, or no-longer-matching certificate performs a
challenge.

## Plan

Use [instant-acme](https://crates.io/crates/instant-acme) with default
features off and its aws-lc-rs feature, matching the workspace crypto
provider so no second provider enters the tree. `deny.toml` gates the new
dependency.

- **Config.** A new `[acme]` section on the relay config: `domains` (the
  certificate's names), `contact` (an email for the account), `dir` (where
  the account key, certificate, and key live; required, no default, since the
  relay has no state directory and packaging already grants
  `/var/lib/moq-relay`), and `directory` (the ACME directory URL, defaulting
  to Let's Encrypt production, with staging documented for rate-limit-safe
  trials). Setting it replaces `listen.tls.cert` / `listen.tls.key`, and
  supplying both is a config error. It also feeds `web.https` when that
  section's cert and key are unset, so one certificate serves the host.
- **Challenge.** HTTP-01 only. The relay's `web.http` router gains
  `/.well-known/acme-challenge/{token}`, answered from the in-flight
  authorization, and config validation requires `web.http` to be bound while
  `[acme]` is set, since the challenge must reach port 80 and `listen.tls`
  is UDP. TLS-ALPN-01 is deliberately not offered: it needs a new TCP :443
  acceptor that collides with `web.https`.
- **Delivery.** The issuer writes the PEM chain and key under `acme.dir` and
  the existing `notify` file watcher swaps them in on the next handshake, so
  no new `moq-tokio` surface is needed and per-worker rotation rides
  [TLS rotation atomicity](/quest/m1/2924-moq-relay-tls-rotation-is-not-atomic-across-thread-per.md).
  Writes are atomic renames into the watched directory, which the watcher
  already treats as a reload. Only the quinn and noq backends on the tokio
  runtime reload certificates today: quiche snapshots its TLS material when
  the listener is built, and the io_uring workers read the certificate once at
  bind. Config validation refuses `[acme]` with those until they can rotate,
  rather than letting a relay serve an expired certificate; lifting the
  refusal is part of whatever gives them a reload path.
- **Startup.** With no cached certificate the relay blocks accepting QUIC
  until the first issuance succeeds; a self-signed placeholder cannot serve
  browsers, so there is nothing honest to serve meanwhile. With a cached
  certificate it starts immediately and renews in the background. A cached
  certificate whose SANs do not cover every configured domain counts as
  missing: an operator who adds or replaces a hostname while reusing
  `acme.dir` gets a fresh issuance, not a wait for the renewal threshold.
- **Renewal.** A daily jittered check reads the cached certificate's expiry
  with the `x509-parser` already used for peer expiry and renews once a
  third of its lifetime remains, Let's Encrypt's own rule for 90-day
  certificates. A failed renewal logs and waits for the next check; it never
  restarts the relay or discards the serving certificate. The account key is
  reused across runs so renewals do not create accounts.
- **Docs.** `doc/bin/relay/config.md` gains the section; `doc/setup/prod.md`
  makes ACME the recommended path and keeps the external-certificate path;
  `doc/bin/relay/http.md` notes the challenge route; the packaging TOML and
  the nix module expose `acme.dir` under the state directory.

Tests: an in-process ACME test server (or the Pebble container behind a
feature flag) issues on first run, a restart with a valid cached certificate
performs no challenge, a cached certificate missing a configured domain is
reissued at startup, a certificate near expiry renews and the QUIC
listener serves the new chain without restart, a renewal failure leaves the
old certificate serving, and the config rejects `[acme]` alongside explicit
paths, without `web.http`, or on a backend that cannot reload.

## Closes

- [#709](https://github.com/moq-dev/moq/issues/709) - close this issue when the quest finishes
