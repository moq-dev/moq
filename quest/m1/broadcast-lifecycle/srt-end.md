# [S] SRT tells a clean end from a failure

## Goal

An SRT ingest whose peer stops sending without a shutdown handshake ends its
broadcast with an error, while a peer that shuts down cleanly finishes it.
Today both finish (`rs/moq-srt/src/server.rs`, the `Ok(())` arm after the
socket stream ends), because srt-tokio ends the stream the same way for both.

## Plan

Decided with the maintainer on 2026-10-23: fix it at the source.

- Find how srt-tokio reports a shutdown handshake versus a peer timeout. If
  it exposes the difference, use it in both the server and the `Client`
  pull (`serve_publish`).
- If it does not, an upstream srt-tokio change is needed: it is outside the
  moq-dev and kixelated orgs, so ask before posting there.
- Test both ends with a mocked clock: a silent peer aborts, a clean shutdown
  finishes.

Public API: none. Wire: none.
