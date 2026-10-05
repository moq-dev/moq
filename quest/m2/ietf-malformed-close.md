# [L] Malformed moq-transport input closes the session

## Goal

On draft-18 and draft-21, every malformed control input that the draft
says ends the session does end it, in both `rs/moq-net` and `js/net`.
It closes with the draft's code, or with PROTOCOL_VIOLATION where a code
is a real burden and the fallback is recorded in
`doc/concept/standard.md`. Today moq-net logs most of these and keeps
going. The cases come from Paul Gregoire's publisher validator
(github.com/mondain/moq-contribution-interop-runner), run in observed mode
against a client built on moq-net 0.17.0. Each case below gives the
runner's scenario name, where it has one:

- An unknown control message type (`receive-unknown-message-type`).
- A message length that doesn't match its body, or a body cut short by FIN
  (`d21-message-body-length-mismatch`,
  `d21-request-message-truncated-at-fin`).
- A zero-length namespace field, or a namespace or full track name over
  4096 bytes.
- A Request ID with the wrong parity: INVALID_REQUEST_ID.
- A GROUP_ORDER outside 1..2.
- An understood key-value with a bad serialization:
  KEY_VALUE_FORMATTING_ERROR
  (`receive-understood-key-value-invalid-serialization`).
- A second GOAWAY on the control stream or on a single request stream, or
  a New Session URI over 8192 bytes: PROTOCOL_VIOLATION. A first GOAWAY on
  a request stream is legal (draft-18 §10.4, draft-21 §9.2) and leaves the
  session up.
- A draft-18 GOAWAY cutoff Request ID with the wrong parity:
  INVALID_REQUEST_ID, not a redirect.
- A server SETUP carrying AUTHORITY or PATH: INVALID_AUTHORITY or
  INVALID_PATH.
- Repeated unknown or GREASE Setup Options are accepted
  (`setup-unknown-grease-options-and-duplicates`,
  `d21-grease-setup-options`). Only a repeated known option is a duplicate.
- A server SETUP AUTHORIZATION TOKEN that is malformed closes with
  KEY_VALUE_FORMATTING_ERROR. A REGISTER that overflows our cache (size 0)
  falls back to USE_VALUE, as draft-21 §9.1.4 requires.
- A SUBSCRIBE_TRACKS prefix with more than 32 fields or over 4096 bytes
  closes with PROTOCOL_VIOLATION, instead of being refused per request
  before it is decoded.

## Plan

Decided with the maintainer on 2026-10-04 and 2026-10-05:

- **Add the missing session codes to the shared registry.**
  INVALID_REQUEST_ID (0x4), INVALID_PATH (0x8) and INVALID_AUTHORITY
  (0x19) become `SessionError` variants (`rs/moq-net/src/error.rs`) and
  `SessionCode` entries (`js/net/src/error.ts`). Their values are the same
  in drafts 18 and 21. That registry is moq-lite's too, and lite codes
  below 32 carry moq-transport's meaning, so add the rows to the Session
  Error Codes table in `drafts/draft-lcurley-moq-lite.md` and to
  `session_codes_round_trip`. No per-version mapping. Where a code is a
  real burden, the PROTOCOL_VIOLATION fallback in the Goal applies.
- **Regression tests, not a CI job for the validator.** Add one session or
  codec test per case, each failing without its fix and asserting the
  draft's code, or PROTOCOL_VIOLATION where the fallback is recorded in
  `doc/concept/standard.md`. Include a positive case: a first GOAWAY on a
  request stream keeps the session open. Run the external runner once by
  hand before the PR is marked ready: observed mode,
  `--publisher-no-fetch`, one scenario per run, on a track of about
  150 kbps. A full-bitrate track floods the runner's event log and aborts
  the run.
- **Mirror in `js/net/src/ietf`** in the same PR.
- Request tokens are out of scope here. AUTH_TOKEN_CACHE_OVERFLOW and the
  request-token decode belong to [Request
  tokens](/quest/m1/auth/request-token.md).

Where each case lives, mapped on 2026-10-04 (paths under
`rs/moq-net/src/`):

- The SETUP/GOAWAY uni stream runs as a task spawned inline in `run_unis`
  (`ietf/session.rs`, around lines 751-874). SETUP and SETUP-parameter
  decode failures there already close the session; only `run_goaway`'s
  error is just logged. Close on the errors `is_protocol_violation`
  (`ietf/subscriber.rs`) marks as the peer's fault, not on every error: a
  reset or transport failure on that stream must stay non-fatal, as
  `died_before_header` keeps it elsewhere. That fixes the second GOAWAY,
  the oversize URI, and an unknown type on that stream. On request
  streams, an error from a follow-up message is only logged at debug in
  `ietf/publisher.rs` (around line 500); promote decode errors there with
  the same filter.
- `died_before_header` (`ietf/session.rs`) treats a body cut short at FIN as
  the stream dying. A frame whose declared length passes FIN is malformed.
  Responses decoded at `ietf/publisher.rs` (about lines 1858 and 1937) lack
  the trailing-bytes check that the first message has.
- Namespace bounds: `ietf/namespace.rs` checks only the 32-part limit. The
  track name in `ietf/subscribe.rs` is unbounded.
- Request ID parity is known only in `Control::new` (`ietf/control.rs`), so
  thread it to where IDs are decoded.
- `ietf/group.rs` maps an invalid GROUP_ORDER, or 0, to Descending.
- KV serialization errors must be told apart from other decode errors.
  `From<&Error> for SessionError` (`error.rs`) maps every `Decode` error to
  PROTOCOL_VIOLATION, and `ietf/session.rs` and `ietf/publisher.rs`
  hard-code it in a few places.
- GREASE: `ietf/parameters.rs` (around line 122) keys every unknown ID as
  `Unknown(u64)` in a HashMap, so any repeat fails as `Duplicate`. In this
  tree that includes the plain duplicate-unknown case the report says now
  passes, so check which build the report ran.
- Client SETUP: `decode_peer_setup` (`ietf/session.rs`) ignores AUTHORITY
  and PATH and never parses the server's token. Reuse `ietf/token.rs`,
  which already does the server-side fallback.
- SUBSCRIBE_TRACKS is refused before its prefix is decoded
  (`ietf/publisher.rs`, about lines 547 and 1609). Decode the prefix first,
  then refuse.

Test models: `an_unknown_uni_type_closes_the_session` and
`an_unknown_bidi_type_closes_the_session` in `ietf/session.rs`, which drive
`ScriptedSession`. Codec tests live in `ietf/parameters.rs` and
`ietf/subscribe.rs`. Add each new decode error to the `ietf_wire` fuzz
regressions where it fits.

Public API: new `SessionError` variants and `SessionCode` entries. Wire:
new moq-lite session codes, with moq-transport's values; moq-transport
behaviour moves closer to the drafts.

## Related

- [Request tokens](/quest/m1/auth/request-token.md) - owns request-token decode and AUTH_TOKEN_CACHE_OVERFLOW
- [Request caps](/quest/m0/request-caps.md) - bounds peer lengths and counts; shares the decode paths
- [moq-transport request codes](/quest/m2/ietf-request-codes.md) - the request-level half of the same validator report
