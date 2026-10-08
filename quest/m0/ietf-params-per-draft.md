# [M] moq-transport parameters follow each draft's lists

## Goal

Every control message accepts exactly the parameters its draft allows, in
`rs/moq-net` and `js/net`, for each supported draft (14 through 22). A
parameter the draft defines for that message is decoded or skipped. Drafts
14 and 15 ignore unknown and misplaced parameters alike; draft-16 closes the
session with PROTOCOL_VIOLATION on an unknown key but ignores a misplaced
one; draft-17 and later close it on both. The case seen in the wild:
moqx's draft-16 SUBSCRIBE_NAMESPACE carries FORWARD (0x10), which we refuse,
and moqx redials in a loop.

## Plan

Triaged from Fastly's moq-relay-interop report (run of 2026-09-23, build
7ee2b02) on 2026-10-07. Facts from the draft text:

- Unknown message parameter: d14 and d15 §9.2 say "Receivers ignore
  unrecognized parameters"; d16 §9.2, d17 §9.3, d18 §10.2 and d22 §9.20
  close with PROTOCOL_VIOLATION.
- A known parameter in a message it is not defined for: d14, d15 and d16
  (§9.2.2) say "it MUST be ignored"; d17 §9.3.1, d18 §10.2.1 and d22
  §9.20.1 close the session.
- d16 allows FORWARD on SUBSCRIBE, REQUEST_UPDATE, PUBLISH, PUBLISH_OK and
  SUBSCRIBE_NAMESPACE. d18 drops it from SUBSCRIBE_NAMESPACE (moved to
  SUBSCRIBE_TRACKS).

Decided 2026-10-07: audit every message against every draft rather than
patch FORWARD alone, since the report's EXPIRES case (fixed in #4195) and
this one are the same class.

Where it lives: the `decode_params!` macro (`rs/moq-net/src/ietf/parameters.rs`,
around line 473) returns `InvalidValue` for any key outside a message's list.
Its call sites are in `fetch.rs`, `publish_namespace.rs`, `publish.rs`,
`subscribe_namespace.rs`, `request_stream.rs`, `request.rs` and
`subscribe.rs`. `Parameters::skip` ignores unknown keys, but only d14
SUBSCRIBE, FETCH and PUBLISH use it; the legacy d14 to d17 decodes of
SUBSCRIBE_NAMESPACE, PUBLISH_NAMESPACE and the `request*.rs` messages go
through `decode_params!`, so they refuse unknown keys on d14 and d15 today
(`test_param_unknown_rejected` asserts that). Those are gaps.

Accepting FORWARD=0 on a d16 SUBSCRIBE_NAMESPACE can be accept-and-ignore:
it only sets FORWARD on the PUBLISH messages it triggers, and we send none
for that subscription (Subscribe Options 0x01).

Coordinate with [moq-transport request codes](/quest/m2/ietf-request-codes.md),
which accepts the delivery-timeout parameters on REQUEST_UPDATE; whichever
lands second drops the overlap.

Tests: one codec test per gap the audit finds, plus one per rule above
(unknown and misplaced, on each side of the d16 and d17 boundaries). Mirror
in `js/net/src/ietf` in the same PR. `doc/concept/standard.md` says an
undefined parameter always closes the session; correct it.

Public API: none. Wire: none new; decoding moves closer to the drafts.

## Related

- [Malformed moq-transport input](/quest/m2/ietf-malformed-close.md) - owns the other session-closing cases
