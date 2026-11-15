# [XS] BBR3's ACK aggregation estimate stays bounded across idle

## Goal

A sender that goes idle between bursts, as media does when the RTT is
shorter than the frame interval, keeps `extra_acked` and the window it adds
bounded by the aggregation it actually sees.

## Plan

`handle_restart_from_idle` (`rs/moq-congestion/src/bbr3/mod.rs`) resets
`extra_acked_interval_start` but not `extra_acked_delivered`, so each
resumed burst counts every earlier epoch's bytes as aggregation. The cap at
cwnd is circular, since `set_cwnd` grows cwnd toward `2·BDP + extra_acked`.
Simulated in the 2026-11-14 audit: 8 Mbit/s media at 30 fps over 20 ms grew
cwnd to 11 MB in 18 s (74 KB with the fix). Draft-06's
`BBRHandleRestartFromIdle` has the same omission; Linux zeroes
`ack_epoch_acked` there ([tcp_bbr.c:540-542](https://github.com/google/bbr/blob/90210de4b779d40496dee0b89081780eeddf2a60/net/ipv4/tcp_bbr.c#L540-L542)).

Zero `extra_acked_delivered` with the interval start, and test that bursts
separated by idle gaps keep `extra_acked` and cwnd bounded. Caused by
[moq-dev/moq#4921](https://github.com/moq-dev/moq/pull/4921), which ported
the draft's omission.
