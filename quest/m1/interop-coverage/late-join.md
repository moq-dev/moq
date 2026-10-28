# [S] Interop covers late join and FETCH

## Goal

On the IETF matrix (`just test interop --tail --versions ietf`), the four
`late join` cells with a native JS publisher (`js-native-node` and
`js-native-bun` to `rust`, drafts 18 and 22) pass. Every other late-join and
CLI FETCH cell already passes there.

## Plan

The late-join lane and the per-publisher CLI FETCH round landed in
`test/interop` (see its README). The four cells fail because
`@moq/web-transport` (0.1.5, the Node and Bun polyfill from
moq-dev/web-transport) surfaces a peer's RESET_STREAM as a plain
`Error("RESET_STREAM: 1")` with no `source` or `streamErrorCode`.
`malformed()` in `js/net/src/ietf/request_stream.ts` then reads the relay's
cancellation of a request (Rust cancels a FETCH by resetting its stream) as a
malformed message and closes the session with PROTOCOL_VIOLATION. Browsers
report a `WebTransportError` with `source: "stream"`, so only native JS is
affected. With resets recognized, all four cells pass.

Fix it in the polyfill: reject reads and writes with a
`WebTransportError`-shaped error carrying `source: "stream"` and the peer's
code, and send the real code from `abort()` and `cancel()` instead of 0.
Release it, bump `@moq/web-transport` here, and rerun
`just test interop --tail --versions ietf`.

Public API: none. Wire: none.
