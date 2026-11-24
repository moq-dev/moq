# [S] A clean end crosses the WebRTC gateway as a clean end

## Goal

A broadcast that ends cleanly on one side of a WebRTC session ends cleanly
on the other. moq-webrtc's WHIP client (`moq export webrtc --connect`) ends
its resource with a `DELETE`, so the WHIP server finishes the ingested
broadcast and serves it for its linger. Its WHEP client
(`moq import webrtc --connect`) finishes its output when the server closes
cleanly, keeping the frames it already received, instead of aborting it.

## Plan

Since [moq-dev/moq#9049](https://github.com/moq-dev/moq/pull/9049), an egress
whose broadcast ends cleanly closes its peer connection (RTCP BYE and DTLS
close_notify) once its last frames are sent; before it, the session never
ended. An ingest reads that close like a vanished peer: `Session::run` aborts
the sink with `session closed`. So a WHIP server aborts a broadcast whose
publisher ended cleanly, and moq's WHEP client aborts its output, which
fails any read of a frame it received but its readers had not reached yet
(seen while writing #9049's tests).

- WHIP client: `client::whip::dial` keeps the answer's `Location` (resolved
  against the dialed URL, keeping its query token), and after `Session::run`
  returns `Ok` sends the `DELETE` (RFC 9725) before returning.
- WHIP server: a publisher's close_notify (str0m's `Event::Closed`) is a
  clean end, like a `DELETE`: the server finishes the broadcast and lingers
  it. A browser's `pc.close()` sends close_notify without a `DELETE`. Only a
  peer that vanishes (ICE consent failure, establishment timeout) aborts.
  Decided on #9049: ✅ both close_notify and `DELETE` end cleanly (rejected:
  `DELETE` only, which RFC 9725 names but which aborts the broadcast of every
  publisher that skips it).
- WHEP client: a WHEP server has no request to end a session with, so the
  server's close_notify is its only clean end. An ingest session that sees
  `Event::Closed` returns as the peer's clean end, and `Client::pull`
  finishes the sink as `AcceptedSession` does after a `DELETE`. The WHIP
  server's ingest takes the same path.

Public API: none. Wire: the WHIP client sends a `DELETE`.
