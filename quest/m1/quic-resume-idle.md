# [M] A resumed QUIC client gives up on its own idle timer

## Goal

A `moq-quic` client whose process was stopped for longer than its idle
timeout (a VM pause, a stalled process) closes the connection within about
one idle timeout of resuming. It does not depend on the relay's stateless
reset reaching it, so clients that resume together don't hang for up to a
minute.

## Plan

Planned 2026-11-09 (maintainer, /quest-import of
[moq-dev/moq#8382](https://github.com/moq-dev/moq/issues/8382)). Milestone:
✅ m1 (rejected: m3, m4).

The report blames the endpoint-wide stateless reset limit (one per 20 ms
without `--listen-reset-secret`, `rs/moq-quic/src/endpoint.rs`). The code
review points at the client, though this is not yet reproduced:

- On resume, the driver (`rs/moq-tokio/src/quic/driver/connection.rs`)
  handles buffered received packets before overdue timers, so the stale
  packets restart the idle timer before `Timer::Idle` can fire.
- The RTT sample (`now - largest_acked_packet_sent`, `rs/moq-quic/src/connection/mod.rs`)
  includes the local stall. The resulting PTO stretches the RFC's
  `max(idle, 3 * PTO)` to about 50 s.

Decided 2026-11-09 (maintainer): ✅ fix the client's root cause (rejected:
client fix plus a token bucket in the reset limiter; token bucket only).
Also rejected:

- An explicit CONNECTION_CLOSE before idle expiry: RFC 9000 §10.1 makes an
  idle close silent, and a stopped peer cannot receive the close anyway.
- Keep-alive jitter: the stop puts the clients in phase, not the timer.

Reproduce first, on mock time if the driver allows it. Use the report's
recipe as the field check: 32 exports SIGSTOPped past a 30 s idle timeout,
then resumed together. If the repro shows the client's timer already fires
in time and only the reset limit delays it, stop and bring that back to the
maintainer instead of changing the limiter.

Public API: none expected. Wire: none.

## Closes

- [#8382](https://github.com/moq-dev/moq/issues/8382) - clients that resume together wait up to a minute for a stateless reset
