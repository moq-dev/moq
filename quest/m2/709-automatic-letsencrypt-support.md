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
  section's cert and key are unset, so one certificate serves the host;
  explicit `web.https` paths win, and the docs say so, since an operator may
  deliberately front HTTPS with a different certificate.
- **Challenge.** HTTP-01 only. The relay's `web.http` router gains
  `/.well-known/acme-challenge/{token}`, answered from the in-flight
  authorization, and config validation requires `web.http` to be bound while
  `[acme]` is set, since the challenge must reach port 80 and `listen.tls`
  is UDP. The relay cannot prove external reachability, so the contract is
  documented instead: the ACME server connects to port 80 of every configured
  domain, and the operator either binds `web.http` there or forwards 80 to
  it. A challenge that never arrives surfaces as the bounded first-issuance
  failure below, naming the domain and the port. TLS-ALPN-01 is deliberately not offered: it needs a new TCP :443
  acceptor that collides with `web.https`. DNS-01 is not offered either, so a
  wildcard in `acme.domains` is a config error rather than a first start that
  blocks forever on an issuance HTTP-01 cannot complete.
- **Delivery.** The issuer writes the key and the PEM chain as one file
  under `acme.dir`, and the existing `notify` file watcher swaps it in on the
  next handshake, so no new `moq-tokio` surface is needed and per-worker
  rotation rides
  [TLS rotation atomicity](/quest/m1/2924-moq-relay-tls-rotation-is-not-atomic-across-thread-per.md).
  One file is what makes the rotation atomic: the watcher reloads both paths
  on every event and refuses a mismatched pair, so separate key and chain
  files would open a window where a new chain meets the old key, and a
  restart inside that window could not build TLS at all. The TLS loader
  therefore accepts a combined PEM for both `cert` and `key`, and the ACME
  output is a single atomic rename into the watched directory. A reload that
  fails keeps serving the last complete pair. With `[acme]` set, failing to
  establish the file watcher is a startup error, since a relay that cannot
  reload would silently serve its certificate to expiry. The account key and the serving key are created
  owner-only (`0600`, the mode `doc/bin/relay/auth.md` already requires of
  private keys) before any bytes land, never with the umask default, and a
  missing `acme.dir` is created `0700`, so a group-searchable parent cannot
  leak a key to another local user. Only the quinn and noq backends on the tokio
  runtime reload certificates today: quiche snapshots its TLS material when
  the listener is built, and the io_uring workers read the certificate once at
  bind. Config validation refuses `[acme]` with those until they can rotate,
  rather than letting a relay serve an expired certificate; lifting the
  refusal is part of whatever gives them a reload path.
- **Startup.** With no cached certificate the relay blocks accepting QUIC
  until the first issuance succeeds; a self-signed placeholder cannot serve
  browsers, so there is nothing honest to serve meanwhile. The attempt is
  bounded per the repository's retry rule: capped exponential backoff with
  jitter inside a startup budget (a few minutes, configurable), after which
  the relay exits with the last real ACME error, the domain, and the port it
  expected the challenge on. With a cached certificate it starts immediately
  and renews in the background. A cached
  certificate whose SANs do not cover every configured domain counts as
  missing: an operator who adds or replaces a hostname while reusing
  `acme.dir` gets a fresh issuance, not a wait for the renewal threshold.
  The directory URL that issued the cached certificate is persisted beside
  it, and a changed `acme.directory` (staging to production, say) also counts
  as missing, so a staging certificate is never served as if trusted.
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
performs no challenge, a cached certificate missing a configured domain or
issued by a different directory is reissued at startup, an unreachable
directory fails the first start inside the budget with an actionable error,
a certificate near expiry renews and both the quinn and noq listeners serve
the new chain through a fresh handshake without restart (the `web.https`
reload is a separate test), a renewal failure leaves the old certificate
serving, a reload of a half-written pair keeps the old one, both generated
keys are mode `0600` after issuance and after renewal, and the config
rejects `[acme]` alongside explicit paths, without `web.http`, with a
wildcard domain, or on a backend that cannot reload.

## Closes

- [#709](https://github.com/moq-dev/moq/issues/709) - close this issue when the quest finishes
