# [L] Legal IETF input never fails the session

## Goal

Every message a conformant draft-14 to draft-22 peer may send decodes, and
anything we don't support is refused per request, not by closing the session.
moxygen and libquicr send several of these today, and Seattle interop is
2026-10-12.

## Plan

Each of these closes the session today with `PROTOCOL_VIOLATION`:

- FETCH on drafts 20+ still decodes the removed Fetch Type field
  (`ietf/fetch.rs`). Decode the draft-20 layout: namespace, name and params,
  with the range in LOCATION_FILTER. Refuse it `NOT_SUPPORTED` until
  [moxygen FETCH](/quest/m1/moxygen/fetch.md) serves it. Fix the encoder and
  the pinned test in `version.rs`.
- Request parameters the draft allows on a message fail `decode_params!`:
  AUTHORIZATION TOKEN (0x03) anywhere, NEW_GROUP_REQUEST (0x32) and the
  object and track filters (0x25 to 0x29) on SUBSCRIBE and REQUEST_UPDATE,
  FILL_TIMEOUT (0x0A) and 0x21 and 0x35 on FETCH, and any parameter on TRACK_STATUS. Accept each
  one where the draft allows it. The token is decoded and ignored, since the
  session grant still applies; [Request token](/quest/m1/auth/request-token.md)
  gives it meaning (decided 2026-09-29). An unknown key stays fatal, per the
  draft.
- INCLUDE_PROPERTIES (0x35) is read as length-prefixed; the draft defines a
  uint8 (`ietf/subscribe.rs`). Encode and decode it as `u8`, and delete the
  doc comment arguing for key-parity framing: that rule covers Setup and
  Properties KVPs, not message parameters.
- FORWARD=0 returns `DecodeError::Unsupported`. Decode it, then refuse the
  request `NOT_SUPPORTED`.

Not in scope: pre-draft-20 subscribe filters other than Largest Object stay
ignored. `older_drafts_are_ignored` in `ietf/publisher.rs` records that
choice, since honoring them would change what existing peers receive.

- Tests: one decode test per message and draft, built from the draft's
  message figures and bytes captured from moxygen and libquicr, plus a
  session test showing each refusal leaves the session open.
- Mirror the decode in `js/net`.

Public API: none. Wire: fixes conformance; no draft change.

## Related

- [Request token](/quest/m1/auth/request-token.md) - also edits `decode_params!`, turning the ignored token into a per-request grant
- [IETF FIN semantics](/quest/m0/ietf-fin-not-cancel.md) - the other interop blocker
- [IETF stream types](/quest/m0/ietf-uni-stream-types.md) - same stream-scoped-before-fatal rule for uni streams
