# [L] Rust clients open PROBE only while a consumer reads it

## Goal

A Rust client opens a PROBE stream only while some `Session::recv_bandwidth()`
consumer is held, and closes it when the last one drops. The handle itself is
never demand, and `Stats` no longer carries `estimated_recv_rate`: the consumer
is the only way to the receive estimate. The PROBE wire is unchanged.

## Plan

Today `moq_net::Session` keeps its own `recv_bandwidth` consumer, so the lite
subscriber's `RecvBandwidth` always sees demand and every session keeps a PROBE
stream open, and moq-tokio's `Connection` holds another across reconnects. The
send side had the same bug, fixed by having the handle hold a weak reference
(`bandwidth::Weak`), and the receive side should follow it.

Decided (2026-10-12, quest-plan):
- Only a held consumer is demand. A `stats()` read is not, because a stats field
  that silently reads `None` unless something else holds a consumer is a trap.
- Remove `Stats::estimated_recv_rate`. That breaks the API, which `main`
  allows. moq-ffi and moq-c replace their `estimated_recv_rate` stats field with
  a receive-bandwidth consumer handle, and every binding and `doc/lib` page
  follows (see the Cross-Package Sync table).
- moq-tokio's `Connection` forwards demand across reconnects instead of holding
  a consumer: it consumes the inner session's estimate only while its own
  consumer is used.

Test: a session nobody holds a consumer for opens no PROBE stream, and one that
holds then drops a consumer opens and then resets it. Reuse the mock transport
stats and the harness in `rs/moq-net/tests/idle_session.rs`.
