# [L] Port BBR3 and the controller callbacks

## Goal

`moq-quic` carries the BBR3 controller from moq-dev/noq, with every
correctness fix the fork has shipped, and it is the default controller. The
fork's BBR regressions pass on `moq-quic`.

## Plan

Move moq-dev/noq's `congestion/bbr3/` over as a unit rather than starting from
quinn#2481: the fork's copy already descends from #2481 and carries the seven
BBR fixes (#4206), the app-limited fix, and the classic ECN CE response
(moq-dev/noq#12). Port the `Controller` extensions it depends on (`PacketId`
and packet-space callbacks, app-limited, cwnd-limited, ACK frequency) by hand;
they are about 350 lines in `connection/mod.rs` and `pacing.rs`.

The fork's roughly 580 lines of regression tests are written against noq's
test harness; rework them onto quinn's rather than porting the harness. Keep
MoQ's config naming controller families (`Loss`, `Delay`), never algorithms.

Verify with the fork's BBR tests and the benchmark matrix against `moq-noq`
1.3.x on the same workloads; report any throughput or latency difference.

## Required

- [Import quinn](/quest/m1/quic/fork/import.md)
