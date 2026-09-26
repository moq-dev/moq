# [M] Make BBR respond to classic ECN

## Goal

BBRv3 responds to validated CE marks before a marking bottleneck has to drop
packets, including during Startup and ProbeUp. Keep ECT(0) enabled and ship
the corrected controller through MoQ's published dependency chain. L4S,
new configuration flags, and changing the default controller are out of scope.

## Plan

The fix lives in moq-dev/noq. In released 1.3.1 (`ff9d2ab5`),
[`on_congestion_event`](https://github.com/moq-dev/noq/blob/ff9d2ab518cfb155f9ebb9925f1c784665eac92a/noq-proto/src/congestion/bbr3/mod.rs#L1828)
passes CE to the lost-packet path with zero lost bytes. Startup and ProbeUp
skip the short-term loss response, while their high-loss checks see no lost
bytes. A controller reproduction sends eight rounds of 1, 2, 4, ..., 128
1200-byte packets, 20 ms apart, acknowledging each round after 10 ms and
reporting CE after `on_end_acks`, as the transport does. Both marked and
unmarked controls remain in Startup with a 318,000-byte window and
42,167,347.2 bytes/s pacing. This is a controller result, not an AQM network
measurement.

[Draft-06 section 3.7](https://www.ietf.org/archive/id/draft-ietf-ccwg-bbr-06.html#section-3.7)
requires an ECN-capable sender to treat CE as congestion, without prescribing
one BBR response. [Google Linux BBRv3](https://github.com/google/bbr/blob/90210de4b779d40496dee0b89081780eeddf2a60/net/ipv4/tcp_bbr.c#L1048)
has a separate Startup ECN response when its ECN mode is eligible. Choose a
classic response consistent with QUIC recovery: stop acceleration and reduce
the permitted sending load on new CE feedback, at most once per recovery
period. Do not fabricate lost bytes or apply repeated reductions for old CE
counts. Preserve the distinction between loss and CE, including undo of
spurious loss. Prefer the existing controller boundary; this fix does not
need a CE-fraction API or Google's L4S policy.

Extend the existing shared BBR `Sim` with the failing reproduction, then test
through actual QUIC ECN validation and controller callbacks. Cover Startup,
ProbeUp, Cruise, ProbeRTT, repeated ACKs with no new CE, several CE-bearing
ACKs in one recovery period, a later period with new CE, simultaneous loss,
and invalid or missing ECN feedback. Acceptance requires a bounded decrease
in sending load under sustained CE, recovery after marking stops, and no
response on the unmarked control; merely changing an internal state is not
enough. Keep the default CUBIC path working.

Retain a reproducible rate-limited marking-versus-dropping network case,
recording queue delay, goodput, loss, and response timing. Run deterministic
regressions in fork CI and the network case at least nightly. Provider
availability does not block this lab validation. Record the response rule
and its recovery-period boundary with the results.

Land the fix in the fork, offer it upstream or record why not, publish an
immutable fork release, and pin the corrected dependency chain here before
completing this quest. Do not wait for the broader QUIC stack release or the
ACK cleanup fix. No wire change or new public API is intended; document any
necessary API change and apply the repository's branch rules. Update stale
controller and relay ECN documentation inline; no separate guide is needed.

## Related

- [ACK cleanup](/quest/m1/bbr-ack-cleanup.md) - an independent fix in the same controller; coordinate ownership of the shared file
- [Loss sampling](/quest/m1/quic/bbr-loss-parity.md) - the separate lost-packet sample repair
- [ECN measurement](/quest/m1/quic/ecn-measure.md) - measures the released response on the backbone
- [L4S](/quest/m2/quic-ecn.md) - a separate scalable ECN policy and opt-in configuration
