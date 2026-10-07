# [M] moq-transport parameters follow each draft's lists

## Goal

Every control message accepts exactly the parameters its draft allows, in
`rs/moq-net` and `js/net`, for each supported draft (14 through 22). A
parameter the draft defines for that message is decoded or skipped; a
parameter defined only for another message is ignored on drafts 14 to 17 and
closes the session with PROTOCOL_VIOLATION on draft-18 and later; an unknown
key still closes it with PROTOCOL_VIOLATION. The case seen in the wild:
moqx's draft-16 SUBSCRIBE_NAMESPACE carries FORWARD (0x10), which we refuse,
and moqx redials in a loop.

## Plan

Triaged from Fastly's moq-relay-interop report (run of 2026-09-23, build
7ee2b02) on 2026-10-07. Facts from the draft text:

- Unknown message parameter: close with PROTOCOL_VIOLATION (d16 §9.2, d18
  §10.2, d22 §9.20). That rule stays.
- A known parameter in a message it is not defined for: d16 §9.2.2 says
  "it MUST be ignored"; d18 §10.2.1 and d22 §9.20.1 close the session.
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
`subscribe.rs`. Draft-14 already skips unknown parameters
(`Parameters::skip`). Accepting FORWARD=0 on a d16 SUBSCRIBE_NAMESPACE can
be accept-and-ignore: it only sets FORWARD on the PUBLISH messages it
triggers, and we send none for that subscription (Subscribe Options 0x01).

Coordinate with [moq-transport request codes](/quest/m2/ietf-request-codes.md),
which accepts the delivery-timeout parameters on REQUEST_UPDATE; whichever
lands second drops the overlap.

Tests: one codec test per gap the audit finds, plus one each for the d16
ignore and d18 close rules on a known-but-misplaced parameter. Mirror in
`js/net/src/ietf` in the same PR.

Public API: none. Wire: none new; decoding moves closer to the drafts.

## Related

- [Malformed moq-transport input](/quest/m2/ietf-malformed-close.md) - owns the other session-closing cases
