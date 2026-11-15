# [S] A requester that walks away reads as a cancel

## Goal

When a requester gives up on a request stream by dropping its reader,
the publisher logs and reports the request as cancelled, not as
`internal error`. A dropped writer already does this.

## Plan

Decided 2026-11-14 (maintainer, /quest-plan): ✅ `main` only, m3 (rejected: a
`release` backport in m0, which changes release-line logs for cosmetics).

Today, dropping a `moq-net` `Reader` drops the transport's receive stream.
QMux's `RecvStream::drop` (`rs/qmux/src/session.rs`) then sends STOP_SENDING
with code 0, which `moq-net` decodes as `StreamError::Internal`
(`rs/moq-net/src/error.rs`, `coding/codes.rs`). For a request whose server is
not graceful, such as track info, `rs/moq-net/src/lite/publisher.rs` then warns
`track info error ... err=internal error`. A dropped `Writer` resets with
`Cancel` (`coding/writer.rs`), so the read side should stop with `Cancel` too.
Check what the QUIC and WebTransport paths send on drop, and match `js/net`.

This bit an operator debugging a transport stall: each request that could never
be answered read as a fault in the track.
