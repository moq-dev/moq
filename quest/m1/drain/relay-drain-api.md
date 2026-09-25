# [M] Relay drain api

## Goal

Expose a drain hook that sends GOAWAY on every relay session. Include new
arrivals while draining, so an embedding process can trigger it on SIGTERM.

Identical behavior on both runtimes: a session served from an io_uring worker
drains the same as one served from a tokio worker.

## Plan

moq.pro's (downstream) fleet drain orchestration consumes this hook: its edge
process sheds the node from GeoDNS, waits out the TTL, then fires the drain.

The signal path already reaches io_uring sessions: `drain_on_signal` fires the
broadcast, per-session `supervise` sends the GOAWAY from the shared runtime,
and `uring::Workers::shutdown` joins the threads only afterwards. What is
missing on both runtimes is the arrival half, and on the io_uring side nothing
proves any of it. Cover the new-arrival GOAWAY on the io_uring accept path
too, and add the drain case to `rs/moq-relay/tests/runtime_uring.rs`: a
session on a worker receives GOAWAY, closes, and the threads join cleanly.
