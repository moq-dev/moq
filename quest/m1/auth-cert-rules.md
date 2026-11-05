# [M] moq auth serve scopes each client certificate by its chain

## Goal

An operator running `moq auth serve` can give different client certificates
different access. Without a restart, a config edit provisions, narrows, or
revokes one certificate, or every certificate under one intermediate CA.
Today every verified certificate gets the same `--mtls-publish` and
`--mtls-subscribe` rules, so two affiliates holding certificates from one CA
get identical access, and scoping them means writing your own auth endpoint.

Not in scope: changing how the relay verifies certificates, or the JWT and
anonymous paths.

## Plan

Decided 2026-11-05 (maintainer, /quest-import of
[#7921](https://github.com/moq-dev/moq/issues/7921)):

- ✅ **Plan it.** Not chosen: won't fix and leave per-certificate scope to
  custom endpoints. The auth contract already carries enough for a leaf
  match (`Peer::fingerprint`, `rs/moq-auth/src/request.rs`), and the
  reporter showed a per-certificate endpoint working, including revocation
  within one re-check. Only the reference server lacks it.
- ✅ **A TOML config like moq-relay's.** `moq auth serve` takes an optional
  config file merged with its CLI flags, the way `moq_relay::Config::load`
  does (`rs/moq-relay/src/config.rs`), so every serve setting can live in
  the file. Not chosen: a `--mtls-dir` of one file per certificate, or a
  side file holding only the per-certificate rules.
- ✅ **Rules key on certificate hashes across the verified chain.** The
  relay sends the SHA-256 of every certificate in the verified chain (leaf,
  intermediates, root) in the auth request's `tls` facts, as an additive
  field beside the existing leaf `fingerprint`. A rule names one of those
  hashes, so it can scope a single leaf or everything under one
  intermediate. The maintainer's words: "per-intermediate, relay sends the
  hash of the entire chain"; this quest reads that as one hash per
  certificate, so any link can be matched. When several rules match, the one
  naming the certificate closest to the leaf wins. Not chosen: per-leaf
  only, `Peer::name` (it folds SAN, CN, and fingerprint into one string), or
  the issuer DN (two CAs can share one).
- ✅ **A certificate no rule matches falls back to the `--mtls-*` rules.**
  Not chosen: refusing it.
- ✅ **A rule can set everything a grant has**: publish and subscribe
  patterns, the cluster-link role, tier, and publish/subscribe rates.
- ✅ **A rule's patterns replace the shared mTLS patterns**, never union, so
  a rule can narrow. Fields it omits inherit the server-wide value. Not
  chosen: each rule being the whole grant.
- ✅ **Reload on change.** The server watches its config file. It swaps in a
  valid edit, and on an invalid one keeps the last valid config and logs an
  error, as moq-tokio's reloaded address map does
  (`rs/moq-tokio/src/listen.rs`). An edit takes effect at each session's
  next re-check (`--revalidate`). Not chosen: re-reading per request,
  SIGHUP, or requiring a restart.
- ✅ m1.

Public API: additive. The CLI flags keep working beside the file, and the
auth request gains one `tls` field. Update the auth
contract docs (`doc/bin/relay/`) and `doc/bin/cli.md`.

Tests: a leaf rule, an intermediate rule, leaf-over-intermediate
precedence, the fallback, an edit narrowing and a delete revoking at the next
re-check, and an invalid edit keeping the last valid config.

## Closes

- [#7921](https://github.com/moq-dev/moq/issues/7921) - `moq auth serve`: scope a client certificate by its identity
