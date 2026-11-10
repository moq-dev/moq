# [L] Our extensions stop riding moq-transport

## Goal

moq-transport sessions no longer negotiate or carry our own extensions:
cluster, hidden, solicit, active count, and the moq-auth extension. Rust
(`rs/moq-net/src/ietf/{cluster,hidden,solicit,active_count,auth}.rs`), JS
(`js/net/src/ietf/{cluster,hidden,solicit,active_count,auth,auth_setup}.ts`),
their handshake wiring, tests, and the moq-transport sections of the
matching `drafts/draft-lcurley-moq-*` documents are deleted. They stay on
moq-lite, where our own peers use them.

## Plan

Part of the [IETF scope freeze](/quest/m2/ietf-scope/README.md), decided
2026-11-09. None of the third-party implementations our interop runs
(Cloudflare, moxygen, imquic, MOQtail, aiomoqt) speaks these extensions, and
our own peers connect over moq-lite.

- Keep the standard AUTHORIZATION TOKEN (`ietf/token.rs`, `token.ts`): it is
  how a third party authenticates.
- Wire: an extension removed here is one a peer must have negotiated, so a
  published client or relay that offers it gets a session without it rather
  than a failure. Verify that against the published `@moq/net` and the
  release relay before deleting each one; if one would break a published
  peer's session, stop and report it.
- A relay forwarding a hidden or soliciting lite broadcast to a
  moq-transport subscriber now treats it as moq-transport does (hidden
  broadcasts are never forwarded there). Test that boundary in both
  languages.
- JS exports such as `Ietf.Cluster` go; the PR's Impact section carries the
  migration note.

Public API: breaking (JS `Ietf` extension types; Rust items under `ietf`
that are public). Wire: moq-transport sessions stop negotiating our
extensions; moq-lite is unchanged.
