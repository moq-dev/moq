# [XS] JS closes an IETF session on an unknown bidi stream type

## Goal

In `@moq/net`, a moq-transport bidi stream whose message type is unknown closes
the session with PROTOCOL_VIOLATION, as Rust and the JS uni path already do,
instead of aborting only that stream. A message a draft defines but we don't
support keeps the refuse-per-request rule
(#4685, #4610).

## Plan

Facts (`origin/main`, 2026-10-01):

- JS: the `default:` arm of the bidi dispatch in
  `js/net/src/ietf/connection.ts` logs and calls `stream.abort(...)`, so the
  `#runBidis` catch never reaches `#violated` and the session stays open.
- JS uni already throws `ProtocolViolation` for an unknown type.
- Rust: `run_dispatch`'s `_` arm in `rs/moq-net/src/ietf/session.rs` returns
  `UnexpectedStream`, which maps to PROTOCOL_VIOLATION. Rust has a uni test
  (`an_unknown_uni_type_closes_the_session`) but no bidi one.

Decided (2026-10-01): unknown is fatal; defined but unsupported is refused per
request. Rejected: making Rust lenient to match JS.

Work: throw `ProtocolViolation` from the JS default arm, and add a bidi test in
both languages. moq-lite is out of scope: its draft says an unknown stream type
MUST be reset and MUST NOT be fatal, and both implementations comply.

Public API: none. Wire: none; fixes conformance.
