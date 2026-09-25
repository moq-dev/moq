# [S] Drain exit

## Goal

`Relay::run` returns as soon as every session has left a drain, instead of
always waiting out the window, and the relay reports which bound ended it:
every session left, or the deadline force-closed the stragglers (and how
many). An orchestrator bounding its stop time can then prove from the log and
`/metrics` which one it hit.

## Plan

Today `drain` in `rs/moq-relay/src/relay.rs` sleeps the whole window plus a
second whatever the sessions do. Counting only needs the sessions that go
through `shutdown::Observer::drain_session` (QUIC on either runtime, and
WebSocket), not the `session::Registry`, which skips LAN peers.

Open question: whether the relay's own outbound cluster sessions count, since
they are not drained by GOAWAY at all.
