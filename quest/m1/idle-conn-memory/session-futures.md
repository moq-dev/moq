# [M] An idle session's tasks and state shrink

## Goal

The futures, tasks and shared state `moq-relay` and `moq-net` hold for each
idle moq-lite session over WebTransport drop well below the 41 KiB they hold
now, found by type size and confirmed with the idle heap profile.

## Plan

The 2026-11-27 idle profile ([epic](/quest/m1/idle-conn-memory/README.md))
put these allocations, per connection, in the session and WebTransport layers:

| Site | KiB |
|---|---|
| `moq_relay::connection::Connection::start`: the boxed `Map<Join<..>>` future | 7.0 |
| the same: the tokio task cell around `moq_tokio::server::Request::ok` | 3.0 |
| `moq_net::server::Server::start_lite`: the boxed `lite::session::Driver` | 7.0 |
| `moq_net::lite::session::start`: `publisher::Shared` and others | 4.0 |
| `kio::Producer<bandwidth::Registry>` default, twice | 1.8 |
| `moq_relay::relay::serve_listening`: the connection task | 1.7 |
| `lite::subscriber` `AnnouncePrefix` occupant `Vec` growth | 1.5 |
| `moq_net::session::Session::new` channels | 1.2 |
| `moq_relay::session::Registry` table | 1.0 |
| `moq_tokio::quic::web::session::Session::new`: `Web` state and its task | 5.4 |
| `transport::boxed::bi` stream boxes | 1.0 |

Start from type sizes (`-Zprint-type-sizes` or `top-type-sizes` on
`moq-relay`) for the futures above: a large future usually holds a rarely
used branch's locals, or a setup value, across an await point. Box that state
or drop it before the long await, so the long-lived future carries only what
an idle session needs. Look at the two `bandwidth::Registry` producers an
idle session builds, and at the `AnnouncePrefix` vector's capacity. Keep the
profile's churn view in sight: a box that runs per frame trades memory for
allocations the fan-out bench counts.
