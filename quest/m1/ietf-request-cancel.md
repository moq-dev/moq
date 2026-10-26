# [M] A cancelled moq-transport request stops writing

## Goal

Cancelling a moq-transport SUBSCRIBE, FETCH, TRACK_STATUS, or namespace
request releases its state at once, even when the peer's send window holds
back the request's first message.

## Plan

Every request write in `rs/moq-net/src/ietf/subscriber.rs` awaits its first
`encode_message` without watching for cancellation, so a peer that withholds
flow-control credit pins a cancelled request.
[moq-dev/moq#6409](https://github.com/moq-dev/moq/pull/6409) already refuses
a request at once when stream credit is exhausted, but an admitted header
still waits for send capacity, and
[moq-dev/moq#6530](https://github.com/moq-dev/moq/pull/6530) landed the Rust
lazy discovery that rewrote these writes. What remains is the awaited first
encode under send-window pressure: watch for cancellation there and reset the
partially written request stream.

Decided 2026-10-25 (maintainer): drop support for the older drafts rather
than handle them here. Drafts 14 and 15 shared one ordered control stream, so
a partial message could not be abandoned; once
[Drop moq-transport 14-17](/quest/m1/ietf-drop/d14-17.md) lands, every
remaining draft gives each request its own stream, and only the reset path
is needed.

Test with a peer that never grants send credit, on drafts 18 and 22.

Public API: none. Wire: none.

## Required

- [Drop moq-transport 14-17](/quest/m1/ietf-drop/d14-17.md) - leaves only per-request streams, so a cancelled request is always a stream reset
