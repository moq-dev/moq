# [S] js/net enforces the input checks Rust does

## Goal

`@moq/net` rejects the inputs `rs/moq-net` rejects, so JS is not the weaker side of a session:

- A GOAWAY redirect is classified on the host actually dialed. `isLocal` in `js/net/src/connection/goaway.ts` parses the opaque `moql:`/`moqt:` host, so `127.1`, `0x7f000001` and `2130706433` pass as non-local while `dialable()` rewrites them to `https:`, which resolves to 127.0.0.1. With `redirect: "follow"` a relay can steer the client and its tokens to loopback.
- `redact` in `js/net/src/util/log.ts` logs scheme, host and port only. For `moql:` URLs `url.origin` is `"null"` and the path, which can carry a token, is kept.
- The lite-01/02 ANNOUNCE_INIT is capped like Rust's (`frame::SEGMENT - 16`) and its count is checked against the announce budget before decoding (`js/net/src/lite/announce.ts`).
- `checkSequence` (`js/net/src/util/sequence.ts`) refuses a group sequence at or above 2^64, as datagrams already do, so `appendGroup` after group 2^64-1 fails instead of minting a group past `UNBOUNDED`.
- `accept.ts` requires a pinned `AcceptProps.version` to be one the client offered, as `rs/moq-net/src/server.rs` does.
- `open.ts` `patience` treats an RTT of 0 as unknown, so a transport that reports 0 before its first sample does not make every stalled open reset a lower-priority stream.

## Plan

Found by the 2026-11-07 invariant audit (js-2, js-5 to js-9). Tests for each, including the three loopback spellings. js-1 (announce queue growth) is already [JS announced coalescing](/quest/m3/js-announced-coalesce.md).
