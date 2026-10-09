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
- A server SETUP AUTHORIZATION TOKEN that is malformed closes with
  KEY_VALUE_FORMATTING_ERROR. A REGISTER that overflows our cache (size 0)
  falls back to USE_VALUE, as draft-21 §9.1.4 requires.
- A SUBSCRIBE_TRACKS prefix with more than 32 fields or over 4096 bytes
  closes with PROTOCOL_VIOLATION, instead of being refused per request
  before it is decoded.

## Plan

Decided 2026-10-08: build on
[#5028](https://github.com/moq-dev/moq/pull/5028) (merged), which rewrote the
same parameter decode and handed this quest one case: a follow-up message on
a request stream that fails to decode.

Decided with the maintainer on 2026-10-04 and 2026-10-05:

- **Add the missing session codes to the shared registry.**
  INVALID_REQUEST_ID (0x4), INVALID_PATH (0x8) and INVALID_AUTHORITY
  (0x19) become `SessionError` variants (`rs/moq-net/src/error.rs`) and
  `SessionCode` entries (`js/net/src/error.ts`). Their values are the same
  in drafts 18 and 21. That registry is moq-lite's too, and lite codes
  below 32 carry moq-transport's meaning, so add the rows to the Session
  Error Codes table in `drafts/draft-lcurley-moq-lite.md` and to
  `session_codes_round_trip`, as [Request
  tokens](/quest/m1/auth/request-token.md) does for 0x13 and 0x17. No
  per-version mapping. Where a code is a real burden, the PROTOCOL_VIOLATION
  fallback in the Goal applies.
- **Regression tests, not a CI job for the validator.** Add one session or
  codec test per case, each failing without its fix and asserting the
  draft's code, or PROTOCOL_VIOLATION where the fallback is recorded in
  `doc/concept/standard.md`. Include a positive case: a first GOAWAY on a
  request stream keeps the session open. Run the external runner once by
  hand before the PR is marked ready: observed mode,
  `--publisher-no-fetch`, one scenario per run, on a track of about
  150 kbps. A full-bitrate track floods the runner's event log and aborts
  the run.
- **Record fallbacks** in the "Deliberate deviations" subsection of
  `doc/concept/standard.md`, dropping the hard-coded count from its intro,
  and update the "Refused, not fatal" bullet above it wherever a case here
  now closes the session.
- **Mirror in `js/net/src/ietf`** in the same PR.
- Request tokens are out of scope here. AUTH_TOKEN_CACHE_OVERFLOW and the
  request-token decode belong to [Request
  tokens](/quest/m1/auth/request-token.md).

Where each case lives, mapped on 2026-10-04 (paths under
`rs/moq-net/src/`):

- The SETUP/GOAWAY uni stream runs as a task spawned inline in `run_unis`
  (`ietf/session.rs`). SETUP and SETUP-parameter decode failures there
  already close the session; only `run_goaway`'s error is just logged. The
  gated server accept, which reads SETUP early, calls `run_goaway` a second
  time (`goaway_recv`) and does the opposite: any error, even a stream reset, ends the session.
  Make both sites agree, ideally by filtering inside `run_goaway`. Close
  on the errors `is_protocol_violation` (`ietf/subscriber.rs`) marks as
  the peer's fault, not on every error: a reset or transport failure on
  that stream must stay non-fatal, as `died_before_header` keeps it
  elsewhere. That fixes the second GOAWAY,
  the oversize URI, and an unknown type on that stream. On request
  streams, a follow-up message (such as REQUEST_UPDATE on a SUBSCRIBE) that
  fails to decode ends only that request with PUBLISH_DONE
  (INTERNAL_ERROR), and the error is logged at debug in `handle_stream`
  (`ietf/publisher.rs`). Close the session there instead, with the same
  filter.
- `died_before_header` (`ietf/session.rs`) treats a body cut short at FIN as
  the stream dying. A frame whose declared length passes FIN is malformed.
  The responses decoded in `advertise_namespace` and `update_namespace`
  (`ietf/publisher.rs`) lack the trailing-bytes check that the first
  message has.
- Namespace bounds: `ietf/namespace.rs` checks only the 32-part limit. The
  track name in `ietf/subscribe.rs` is unbounded.
- Request ID parity is known only in `Control::new` (`ietf/control.rs`), so
  thread it to where IDs are decoded.
- KV serialization errors must be told apart from other decode errors.
  `From<&Error> for SessionError` (`error.rs`) maps `TooManyParameters` to
  KEY_VALUE_FORMATTING_ERROR but every `Decode` error to
  PROTOCOL_VIOLATION, and `ietf/session.rs` and `ietf/publisher.rs`
  hard-code it in a few places.
- Client SETUP: `decode_peer_setup` (`ietf/session.rs`) ignores AUTHORITY
  and PATH and never parses the server's token. Reuse `ietf/token.rs`,
  which already does the server-side fallback.
- SUBSCRIBE_TRACKS is refused before its prefix is decoded (`handle_stream`
  in `ietf/publisher.rs`). Decode the prefix first, then refuse.

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
- [moq-transport request codes](/quest/m2/ietf-request-codes.md) - the request-level half of the same validator report
