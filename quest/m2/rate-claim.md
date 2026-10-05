# [S] Bitrate caps in tokens and grants

## Goal

A token or auth-endpoint grant can cap a session's upload and download
bitrate, and a relay that cannot enforce the cap refuses the session instead
of admitting it uncapped. This lands the wire shape and the refusal; the
transports enforce it in their own quests.

Asked by an app developer who hands MoQ tokens to their own end users and
needs "1080p on the basic plan" to hold against a modified client. Caps bound
bytes, not resolution or framerate: the catalog is the client's word, bytes
are what cost money, and 8K or 9999 fps both blow a byte budget.

## Plan

Decided in the 2026-10-04 plan:

- **Nested on publish and subscribe.** Each accepts its current pattern or
  list, or an object `{"path": <pattern or list>, "rate": <bits/s>}`.
  `publish.rate` caps every byte the client sends on the connection,
  `subscribe.rate` every byte it receives. One object per direction; a
  per-pattern budget is a non-goal. Absent `rate` is uncapped.
- **Bits per second**, matching encoder settings (`-b:v 8M`).
- **Per connection.** N connections on one token get N x the cap; the
  per-token session limits bound N.
- **Burst is fixed, not on the wire**: about one second of `rate`, so a
  keyframe does not trip it. Tuning it later needs no claim change.
- **Unions take the highest cap.** When in-band auth unions several tokens
  on one session, the session's cap per direction is the highest among them,
  and an uncapped token lifts it, matching the union's permissive meaning.
- **Fail loud through the shape.** A capped grant is never written as legacy
  `put`/`get` (an old reader would drop the cap); the object form fails to
  parse as `Patterns` on an old relay or SDK, so it refuses the whole token.
  The same holds for `Grant` from an auth endpoint, which does not deny
  unknown fields, so the cap must stay inside `publish`/`subscribe`.
- **Refuse until enforced.** The relay admits a capped session only on a
  transport that enforces caps. Until [QUIC caps](/quest/m2/rate-quic.md) and
  [WebSocket caps](/quest/m2/rate-websocket.md) land, every capped session
  is refused with a clear error. Embedders that run their own gateways
  (moq.pro's WHIP, SRT, RTMP, HLS) must check the cap themselves; the release
  notes call this out.
- Key scope (`Scope`) ignores `rate`: a signing key bounds paths, not
  bitrate.

Rust (`rs/moq-auth`: `wire.rs`, `claims.rs`, `grant.rs`) and JS
(`js/auth`) together, with round-trip vectors shared between them. Update
`doc/bin/relay/auth.md`, `doc/lib/rs/moq-auth.md`, and `doc/lib/js/auth.md`
inline.

Tests: the object form round-trips in both languages; a capped token never
encodes as `put`/`get`; a published `moq-auth` reader refuses a capped token;
a union's cap is the highest; the relay refuses a capped session today.

## Related

- [QUIC caps](/quest/m2/rate-quic.md) - the first transport to enforce it
- [In-band auth](/quest/m1/auth/README.md) - the token union the highest-cap rule follows
