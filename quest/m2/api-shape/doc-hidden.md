# [M] Ban doc(hidden) in Rust

## Goal

`just check` fails on any `#[doc(hidden)]` in the repository's Rust, vendored
code included. Each current use becomes private (`pub(crate)` or narrower), or
documented public API. A public item is either documented or not public.

JS `@internal` is out of scope: it is how a JS package shares code across its
own modules without publishing it, which TypeScript has no visibility for.

## Plan

Planned 2026-11-09 (maintainer, /quest-complete follow-up of
[moq-dev/moq#8369](https://github.com/moq-dev/moq/pull/8369), which un-hides
moq-net's route stream: "audit any other doc(hidden). In fact just ban it").

Decisions:

- ✅ The ban covers `rs/moq-webrtc/src/vendor` too (rejected: exclude vendored
  code). Those modules are private and already trimmed by hand, so their
  `doc(hidden)` hides nothing and can be deleted. `vendor/mod.rs` records
  upstream revisions, not local edits, so the removal needs no note there.
- ✅ Feature-gated test support stays `pub` behind its feature and is
  documented, like tokio's `test-util` (rejected: make it private by moving
  bench and fuzz bodies into the crate). This covers `pub mod fuzz` (moq-net,
  moq-quic, moq-mux, moq-rtmp), `pub mod sim` (moq-quic, moq-tokio, and
  `quic::driver::sim`), and the bench exports (`moq_json::bench`,
  `moq_stats::produce::bench`, `moq_quic::connection::BenchSentPackets`). Each one's doc
  line says it is test support and names the feature that enables it.
- ✅ Rust only (rejected: audit or ban JS `@internal`).
- ✅ m2, in this epic (rejected: m3 with the audits).

Guidance for the rest: `moq_tokio::settings` (used by moq-relay and
moq-bench), `moq_quic::mux` and its `connection::mux` re-export, and
`moq_webrtc::p2p` follow this epic's rules: private when only the crate uses
it, deleted when nothing does, otherwise documented. The two hides on moq-net's
route stream go with #8369; if it has not landed when this is worked, remove
them here.

Add the check where `just check` already scans tracked text (`sh/text.sh`
greps every tracked file for em dashes), so it runs in the merge gate. Update
[wire fuzz coverage](/quest/m4/wire-fuzz-coverage.md), which asks for a
"hidden `fuzz` module", and [the relay over QUIC on the sim](/quest/m4/quic-sim.md),
which describes moq-tokio's "hidden `sim` feature", to match.

Public API: newly documented items in moq-net, moq-quic, moq-mux, moq-rtmp,
moq-json, moq-stats, moq-tokio, and moq-webrtc; anything made private or
deleted is breaking. Wire: none.

## Related

- [Lint audit](/quest/m3/audit-lints.md) - the wider lint configuration
