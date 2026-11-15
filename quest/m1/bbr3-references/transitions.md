# [XS] BBR3 clears congestion signals in Drain and floors its Reno probe time

## Goal

Leaving Startup charges a loss or CE mark once, and a small-BDP bulk flow
probes no more often than Linux's minimum of a few rounds.

## Plan

- `enter_drain` (`rs/moq-congestion/src/bbr3/mod.rs`) does not reset the
  congestion signals as Linux does on Drain entry
  ([tcp_bbr.c:1963](https://github.com/google/bbr/blob/90210de4b779d40496dee0b89081780eeddf2a60/net/ipv4/tcp_bbr.c#L1963)),
  so the CE that ends Startup also cuts `bw_shortterm` and
  `inflight_shortterm` one loss round later in Drain, contradicting the
  ledger's once-per-episode CE response. Call `reset_congestion_signals()`.
- `is_reno_coexistence_probe_time` divides `min(BDP, cwnd)` by the MSS,
  rounding down, with no floor: at 1 Mbit/s and 20 ms it probes every one or
  two rounds, and DOWN can end before draining the probe's queue. Linux's
  target goes through `bbr_inflight`, floored by the quantization budget
  ([tcp_bbr.c:1010-1015](https://github.com/google/bbr/blob/90210de4b779d40496dee0b89081780eeddf2a60/net/ipv4/tcp_bbr.c#L1010-L1015)).
  Floor it at `max(bdp, offload budget, min_pipe_cwnd)` and round up.
  Application-limited flows are unaffected, since their UP lasts until loss.

Test each. Caused by [moq-dev/moq#4921](https://github.com/moq-dev/moq/pull/4921),
which ported both from draft-06.
