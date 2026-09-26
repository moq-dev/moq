# [M] Make BBR respond to classic ECN

## Goal

BBRv3 responds to validated CE marks before a marking bottleneck has to drop
packets, including during Startup and ProbeUp. Keep ECT(0) enabled and ship
the corrected controller through MoQ's published dependency chain. L4S,
new configuration flags, and changing the default controller are out of scope.

## Plan

[moq-dev/noq#12](https://github.com/moq-dev/noq/pull/12) makes the fix.
BBR now responds to new CE feedback at most once per RFC 9002 recovery
episode. Startup exits and drains. Refill and ProbeUp stop the probe and
bound `inflight_longterm` by the marked packet's inflight. Other states take
the short-term loss cut at the end of the round. CE adds no lost bytes, and
an episode that saw CE is not undone as spurious. The PR records the rule,
the tests, and the results. On a simulated QUIC upload over a one-BDP buffer,
the marking run no longer drops, and its mean queue falls from 7.4ms to
3.2ms at about the same goodput.

What remains:

- Merge the fork PR and publish the next fork release, which is 1.3.2 unless
  something breaking lands first.
- Pin `moq-noq-proto`, `moq-noq-udp`, and `web-transport-moq` to that
  release in `Cargo.toml` and `Cargo.lock`, then check with `just check`.
- The fork PR records why the fix is not offered upstream yet: it builds on
  the fork-only `PacketId` callbacks and the undo series. It goes upstream
  with that series.
- Complete this quest with the pin. No moq docs describe ECN behavior, so
  none need updating.

## Related

- [ACK cleanup](/quest/m1/bbr-ack-cleanup.md) - an independent fix in the same controller; coordinate ownership of the shared file
- [Loss sampling](/quest/m1/quic/bbr-loss-parity.md) - the separate lost-packet sample repair
- [ECN measurement](/quest/m1/quic/ecn-measure.md) - measures the released response on the backbone
- [L4S](/quest/m2/quic-ecn.md) - a separate scalable ECN policy and opt-in configuration
