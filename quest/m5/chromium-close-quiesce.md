# [S] Send a WebTransport close's capsule without waiting for the peer's resets

## Goal

A WebTransport session's close sends its `CLOSE_WEBTRANSPORT_SESSION` capsule
at once again, instead of a round trip later (or a grace period later, for a
peer that never resets the streams it was asked to stop).

## Plan

`moq-tokio`'s `Session::close` and `moq-uring`'s web `close` first call
`poll_quiesce` (`rs/moq-quic/src/connection/mod.rs`): it stops the writers of
every stream the server sends on and waits until the peer acknowledged what
they wrote, then sends a reset with `WT_SESSION_GONE` on each, and STOP_SENDING
with it on every stream the peer may still send on, the HTTP/3 CONNECT,
control, and QPACK streams aside. They write the
capsule only once the peer has acknowledged the resets and reset each stopped
stream, or once the close's grace runs out. This avoids a Chrome renderer
crash: a capsule arriving while the page has a bidirectional stream with a
read pending and a unidirectional stream with a write in flight kills the tab
with a null-pointer read. Stopping the page's streams alone was not enough:
the page kept reads pending on the server's streams, and the tab still died
on a revocation's close. The reproduction, a 100-line aioquic
server and a 30-line page with no MoQ, is attached to the Chromium issue below.

Once a shipping Chrome carries the fix, drop the wait from both closes, along
with `poll_quiesce`, `abandon_quiesce`, `reset_quiesced` and the drivers'
wrappers and `quiesce_keep`s, `StreamsState::sending`, `resettable`,
`quiesced`, `stop_for_writer`, and `release_quiesced_resets`, the `quiesced`
field on moq-quic's `Recv`, `Stop::Quiesce` and `Held::UntilQuiesced` on its
`Send`, the reset of a quiesced stream on the peer's STOP_SENDING, the wakes of
the `ended` list on an accept, an acknowledgment, and a reset dropping
unacknowledged data, the `ended` waiter list itself, the close's
classification of the peer's streams (`Classifying` in moq-tokio), and their
tests (`tests/quiesce.rs`,
`close_waits_for_the_peer_to_reset_its_streams` in moq-tokio, and
`a_close_waits_for_the_peer_to_reset_its_streams` in moq-uring), unless
something else uses them by then. Run the reproduction against that Chrome
before deleting.

Wait for the fix to reach Chrome stable, not only Canary: the wait protects
every user still on an affected release.

## Waiting

- Chromium: fixes [issue 589817008](https://issues.chromium.org/issues/589817008)
  and the fix reaches Chrome stable. Check the issue's status and its fixed-in
  milestone, then run the reproduction on that stable release.

## Related

- [A Chromium patch stops a WebTransport close from crashing the tab](/quest/m4/browser-fixes/chromium-close-crash.md) - the fix we write for issue 589817008
