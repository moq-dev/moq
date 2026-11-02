# [S] The shared pacer holds its rate under 2 Mbit/s

## Goal

Under 2 Mbit/s, a rate-paced controller on moq-quic (BBR3 today, and any candidate that sets a pacing rate) sends at its pacing rate, not up to twice it. BBR3 is retuned in the same PR so the nightly probe scenarios, `competing_cubic` included, stay within their late-frame and overhead budgets.

## Plan

Found by [moq-dev/moq#7546](https://github.com/moq-dev/moq/pull/7546) (the GCC candidate). Below 2 Mbit/s the pacer's burst budget is one packet, and moq-quic checks a datagram's pace while the packet before it is still being built, so a batch of two leaves on one packet's credit. The shared pacer (`rs/moq-congestion/src/pacer.rs`) then forgets the overshoot instead of charging it to the next send. #7546 fixed it by carrying the overshoot as debt (its commit `b62e538b94`), which moved BBR3's nightly `competing_cubic` (`rs/moq-quic/src/tests/probe.rs`) to 441.5 late frames against an allowed 439.4, so it reverted the fix rather than retune BBR3 there.

Decided 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-afk run): m1, because the next release ships BBR3 as moq-quic's default and a 2x overshoot at low rates is a correctness bug in what it sends. Land the pacer fix and BBR3's retune together, so the nightly never runs one without the other.

- Start from `b62e538b94`'s debt approach, with a unit test in `pacer.rs` that a batch outrunning a one-packet budget is charged to the next send, and the existing `aggregate_throughput_matches_controller_pacing_rate` swept below 2 Mbit/s.
- Retune where the change shifts BBR3's probing, not the budgets: the probe scenarios' budgets judge the probe, and loosening them to pass would hide the fix's effect. Report every probe scenario's paired late frames, p99, and overhead before and after, on the scenario's full seed set.
- `competing_cubic`'s 8-seed late budget is fragile on its own ([moq-dev/moq#7538](https://github.com/moq-dev/moq/pull/7538) found 28% of 8-seed windows fail it); its seed count is planned separately in [moq-dev/moq#7440](https://github.com/moq-dev/moq/pull/7440). Judge this change on a seed set large enough to separate it from that noise, not on whether seeds 1-8 pass.
- Re-run the realtime candidates' rows that pace below 2 Mbit/s ([GCC](https://github.com/moq-dev/moq/pull/7546), [BBR3 delay bound](https://github.com/moq-dev/moq/pull/7544), [SCReAMv2](https://github.com/moq-dev/moq/pull/7547)) for whichever have landed, and record the shift in the [epic](/quest/m2/quic-realtime/README.md).

Public API: none. Wire: none.

## Related

- [Realtime uplink congestion control](/quest/m2/quic-realtime/README.md) - every candidate paces through this pacer
- [Probe up to a target bitrate](/quest/m2/probe/README.md) - its scenario budgets are what the retune must keep
