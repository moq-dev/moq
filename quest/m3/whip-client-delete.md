# [S] A WHIP client ends its resource with a DELETE

## Goal

`moq export webrtc --connect` (moq-webrtc's WHIP client) sends a `DELETE` to
the resource URL the server returned once its broadcast ends cleanly, so the
WHIP server finishes the ingested broadcast and serves it for its linger
rather than aborting it.

## Plan

Since [moq-dev/moq#9049](https://github.com/moq-dev/moq/pull/9049), a WHIP
client whose broadcast ends cleanly closes its peer connection (RTCP BYE and
DTLS close_notify) and returns `Ok`; before it, the client never ended. The
WHIP server reads that close like a vanished peer: `Session::run` aborts the
sink with `session closed`, so viewers see an abort instead of a finished
broadcast. RFC 9725 ends a session with an HTTP `DELETE` on the `Location`
the `POST` returned, which the server already treats as a clean end
(`AcceptedSession::run`).

Recommended: `client::whip::dial` keeps the answer's `Location` (resolved
against the dialed URL, keeping its query token), and after `Session::run`
returns `Ok` sends the `DELETE` before returning. Rejected: treating a
peer's close_notify as a clean end on the server, which RFC 9725 does not
ask for and would finish a broadcast a browser abandoned with `pc.close()`.

Public API: none. Wire: none.
