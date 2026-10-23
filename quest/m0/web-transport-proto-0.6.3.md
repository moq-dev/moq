# [XS] Take web-transport-proto 0.6.3

## Goal

The `main` and `release` lockfiles resolve web-transport-proto 0.6.3, which
bounds decoded CONNECT headers to a 16 KiB field section, counting each field
as name + value + 32 bytes. Until then, a peer can make moq-tokio, moq-uring,
or the iroh backend hold about 2.7 MB per session from one 64 KiB CONNECT,
before auth: unique short literal names amplify the frame about 42x (the
[QPACK header accordion](https://foxio.io/blog/header-accordions-qpack-amplification-attacks-against-http-3-servers)
class, without its dynamic-table variant, which the decoder refuses).

Waiting on moq-dev/web-transport's
[Bound decoded CONNECT headers](https://github.com/moq-dev/web-transport/blob/main/quest/a0/qpack-field-section.md)
quest. Check: `cargo info web-transport-proto@0.6.3` resolves.

## Plan

Decided 2026-10-21 (maintainer):

- **Fix upstream, not in-tree.** The iroh backend decodes CONNECT inside
  web-transport-iroh with the same crate, so only an upstream release covers
  every backend, and web-transport-proto stays upstream
  ([Drop web-transport-trait](/quest/m2/drop-web-transport-trait.md) records
  why). This is the one exception to taking no work in moq-dev/web-transport.
- **Lock-only bumps on both lines.** `cargo update -p web-transport-proto` in
  a PR against `main` and a backport PR against `release`, whose
  `web-transport-proto = "0.6"` also takes 0.6.3, so moq.dev's relay gets the
  bound at its next patch release. No `Cargo.toml` change.
- **No moq-side test.** The upstream unit test covers the decoder. Confirm
  both servers' SETTINGS now carry `MAX_FIELD_SECTION_SIZE`; they build them
  with `Settings::enable_webtransport`, which 0.6.3 extends.
- moq-uring's 64 KiB `MESSAGE_LIMIT` stays: it also bounds SETTINGS, and
  0.6.3's sync decoder refuses an oversized HEADERS frame at its declared
  length, before the payload is buffered.
