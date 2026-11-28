# [M] An idle session's tasks and state shrink

## Goal

The futures, tasks and shared state `moq-relay` and `moq-net` hold for each
idle moq-lite session over WebTransport drop well below the 41 KiB they held
on 2026-11-27, found by type size and confirmed with the idle heap profile.

## Plan

The 2026-11-27 idle profile ([epic](/quest/m1/idle-conn-memory/README.md))
put these allocations, per connection, in the session and WebTransport layers.
A first pass (sizes read with `RUSTC_BOOTSTRAP=1 RUSTFLAGS=-Zprint-type-sizes`,
which prints every future's layout on stable) shrank these, so a future's
size is its allocation, in jemalloc size classes:

| Site | KiB then | Now |
|---|---|---|
| `Connection::start`'s boxed `supervise` future | 7.0 | 4.0 |
| the task around `Request::ok`'s driver | 3.0 | 0.75 |
| `lite::session::Driver` | 7.0 | 3.5 |
| the relay connection task (`Lifecycle` held `Cluster` inline) | 1.7 | 0.9 |
| `web::Session::run_recv` task | 2.0 | 1.0 |

Sampled live heap on loopback macOS (2,000 idle connections) fell from about
105 KiB to about 99 KiB per connection, and RSS from 116-118 to 106-109 KiB.

What remains, with the profile's figures:

| Site | KiB |
|---|---|
| `moq_net::lite::session::start`: `publisher::Shared` and others | 4.0 |
| `moq_tokio::quic::web::session::Session::new`: `Web` (2.1 KiB inline: `SessionAccept` 1.0, `Settings` 0.5, request 0.2) | 5.4 |
| `moq_net::bandwidth::Registry` and `Viewers`, one of each per session | 1.8 |
| `moq_net::session::Session::new` channels | 1.2 |
| `moq_relay::session::Registry` table | 1.0 |
| `transport::boxed::bi` stream boxes | 1.0 |
| `lite::subscriber` `AnnouncePrefix` occupant `Vec` growth (a `kio::Tasks` reserves four slots for a machine under 1 KiB; a first-slot reservation costs a second allocation for sets that reach two, so measure churn first) | 1.5 |
| `supervise` is 3.9 KiB, just over the 3.5 KiB class: `Lease` is 2 KiB inline, the select's futures 1 KiB | 0.5 |

Method notes for the rest: an `async fn` stores each argument twice (as a
capture and a local), so a long-lived future is built from combinators or an
`async move` block over captures instead; a handle cloned per machine holds
one `Arc` of its fields; and the `-Zprint-type-sizes` build above, plus an
idle run of `moq-bench`, is the loop. `jeprof` on macOS cannot unwind or
symbolize the relay (no `dyld_info`, no frame pointers in jemalloc's
backtrace), so per-site attribution needs a Linux host.

Once the epic's `idle` workload lands, run `just bench-profile heap` on a Linux
host and record the per-site table here, so the remaining rows are attributed
by profile instead of by type size.
