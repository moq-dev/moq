# An idle connection costs the relay less memory

## Goal

`just bench-profile heap` can profile an idle-connection workload and report
the relay's resident and heap bytes per connection, so each fix below shows
its saving and a regression shows on the next run. The children shrink the
parts of an idle connection's footprint that a heap profile attributes.

## Plan

Measured 2026-11-27 at `13be14703e` on loopback: `moq-relay` built with
`--features jemalloc` and the `profiling` profile, 5,000 `moq-bench`
connections that publish and subscribe to nothing (`--broadcasts 0
--subscribe 0 --frame-size 0 --group-size 0`), moq-lite over WebTransport,
BBRv3 (`--quic-congestion-control bbr`), `--auth-public ""`, WebSocket
fallback off. The relay held **93 KB of RSS per idle connection** without
profiling, 40 s after the last connect. At the earlier `80cafb3afe` the same
run held 187 KB per connection, so the footprint already halved in between.

A `MALLOC_CONF=prof:true,prof_active:true,lg_prof_sample:14` run, with a
`SIGUSR1` dump before the load and one at steady state diffed by
`jeprof --base`, put the sampled live heap at 104.5 KiB per connection. Each
allocation is filed under the leaf-most frame naming a layer:

| Layer | KiB/conn | Largest sites | Owner |
|---|---|---|---|
| moq session tasks and state | 34.6 | `moq_relay::connection::Connection::start` future 7 KiB + task 3 KiB, `lite::session::Driver` 7 KiB, `lite::publisher::Shared` 1 KiB, bandwidth `Registry` 1.8 KiB | [session-futures](/quest/m1/idle-conn-memory/session-futures.md) |
| QUIC driver (`moq-tokio`) | 25.6 | `ConnectionInner` 10 KiB (holds moq-quic's `Connection`), the connection's event channel 12.6 KiB | [quic-state](/quest/m1/idle-conn-memory/quic-state.md) |
| QUIC connection (`moq-quic`, `moq-congestion`) | 14.3 | sent-packet slot chunk 4.2 KiB, BBRv3 1.5 KiB, datagram schedule 1.4 KiB | [quic-state](/quest/m1/idle-conn-memory/quic-state.md) |
| TLS (rustls) | 13.5 | packet keys 5 KiB, handshake buffer 2.6 KiB, `TlsSession` 1.4 KiB | [quic-state](/quest/m1/idle-conn-memory/quic-state.md) |
| Auth (`moq-net`, `moq-relay`, `moq-auth`) | 9.9 | one-entry `BTreeMap` leaves: `auth::State::links` 3.5 KiB, `Refresh::credentials` 3.1 KiB | [auth-state](/quest/m1/idle-conn-memory/auth-state.md) |
| WebTransport session (`moq-tokio`) | 6.6 | `web::Session` state and task 5.4 KiB | [session-futures](/quest/m1/idle-conn-memory/session-futures.md) |

The parts no child owns are needed: the current and next 1-RTT key
generations, the core connection and congestion state, and one task per
connection.

This README's own work is the bench workload. The heap mode refuses a run
whose churn holds no MoQ frames, which an idle run never has, and reports
neither connection count nor bytes per connection. Add an `idle` workload
that holds a fixed connection count with no traffic, take the
`steady-end` snapshot after the last connect has settled, and report RSS and
sampled heap per connection beside the existing retained totals. A child can
land before it, measured by hand as above.
