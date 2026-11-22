# [M] Fix JS qmux, auth, IETF close-reason/varint, and stats drain bugs

## Goal

`@moq/qmux` handles post-cancel messages and illegal stream-direction/limit
frames cleanly, `@moq/auth` projects asymmetric private keys on verify and
validates JWT JSON/UTF-8 strictly, `@moq/net` IETF preserves `ProtocolViolation`
close codes on long reasons and decodes 62-bit `EndOfRange` fields, and
`@moq/stats` drains final snapshot frames before closing.

## Plan

Decided 2026-11-21 (maintainer, /quest-plan of the exhaustive pre-cut
full-codebase audit): ✅ place high-severity and impactful JS protocol/security
bugs in `m1` (rejected: `m0` with `release` backports; `m3`).

1. **`js/qmux/src/web-socket-stream.ts:206-211,253-263`**:
   - Guard `ws.onmessage` after `readable.cancel()` so late WebSocket frames do
     not enqueue into a closed controller and throw an uncaught `TypeError`.
2. **`js/qmux/src/session.ts:93-97,1077-1082,1122-1124,1495-1511` &
   `js/qmux/src/frame.ts:620-625`**:
   - Reject illegal stream-direction control frames with `STREAM_STATE_ERROR`
     and enforce the RFC 9000 `<= 2^60` stream count limit.
3. **`js/auth/src/set.ts:104`, `js/auth/src/key.ts:207-214,481-485`**:
   - Project asymmetric private keys to their public half in `KeySet.verify`
     (parity with `rs/moq-auth`).
   - Decode JWT payloads with fatal UTF-8 (`TextDecoder(..., { fatal: true })`)
     and `Json.parse` (rejecting duplicate JSON keys).
4. **`js/net/src/ietf/{connection.ts:281-294,421-423,request_stream.ts:268-272,subscriber.ts:499-506}`
   & `js/net/src/connection/accept.ts:79-84`**:
   - Wrap WebTransport close reasons with `closeReason(...)` so a >1024-byte
     error reason does not cause `WebTransport.close()` to throw and downgrade
     `ProtocolViolation` to a clean close code `0`.
5. **`js/net/src/ietf/object.ts:549-559` & `js/net/src/ietf/range.ts:118-145`**:
   - Decode `EndOfRange.object` and `flags` with `c.u62()` (`bigint`) and throw
     `ProtocolViolation` on unknown flag bits.
6. **`js/stats/src/consumer.ts:183-186`**:
   - Drain the active snapshot consumer to EOF on `TRACKS_FINISHED` instead of
     cancelling early and dropping the final snapshot frame(s).

Public API: none. Wire: RFC 9000 / IETF MoQ / JWT strictness fixes. Add unit
tests in `js/qmux`, `js/auth`, `js/net`, and `js/stats`.
