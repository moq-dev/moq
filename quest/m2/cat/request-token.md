# [S] A token on any message but SETUP is refused

## Goal

The `AUTHORIZATION TOKEN` parameter (`0x03`) on SUBSCRIBE, REQUEST_UPDATE,
PUBLISH, FETCH, PUBLISH_NAMESPACE, SUBSCRIBE_NAMESPACE, TRACK_STATUS, and
every other message whose decoder reads parameters decodes and is refused
with the request's error path, so the session survives and the log says why.
Today the strict decoder on newer drafts fails the whole message on the
unknown key, and the legacy drafts silently ignore it.

## Plan

- Refuse with `Unsupported`, mapped through `to_code` for the draft, naming
  the parameter. Per-operation authorization is not planned: renewal is the
  [in-band auth](/quest/m1/auth/README.md) line's AUTH exchange.
- Both decoder families change: the strict `decode_params!` path on newer
  drafts, which rejects the key today, and the generic KVP path the legacy
  drafts use, which keeps an odd key and lets the caller ignore it.
- `js/net` mirrors the refusal.
- Tests: a token on SUBSCRIBE is refused and the session continues, on one
  legacy draft and one strict draft, in Rust and JS.

Public API: none. Wire: none new.

## Required

- [Setup token](/quest/m1/setup-token.md) - supplies the token decoder
