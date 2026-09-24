# [S] IETF subscriptions end cleanly

## Goal

A moq-transport subscription whose publisher finishes the track ends cleanly
for its subscriber, on every negotiated draft, the way it does over moq-lite.
Today it ends in error.

## Plan

Found while testing announce-to-serve over the mock session
(`rs/moq-net/tests/announce_to_serve.rs`): a finished track arrives whole,
then the subscriber reports `short buffer` instead of the end. The Rust
publisher writes PUBLISH_DONE before closing the subscribe stream, and the
Rust subscriber's `run_subscribe` waits on `stream.reader.poll_closed`, which
treats any trailing bytes as a decode error, so it never reads the message.
Decode PUBLISH_DONE and map its status to a clean finish or an abort. Check
`js/net`'s IETF subscriber for the same gap.

Once fixed, the IETF case in `announce_to_serve.rs` stops special-casing the
track's end: it should match the local and moq-lite runs exactly.
