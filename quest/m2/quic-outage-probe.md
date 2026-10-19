# [S] Probes resume when the peer is heard again

## Goal

After an outage, a QUIC sender whose probe timeout has backed off sends its
next probe as soon as it receives a packet from the peer, instead of
waiting out the backed-off timer. A 5 s outage costs about the outage plus
an RTT, not several seconds more, by default and within RFC 9002.

## Plan

Decided 2026-10-19 by the maintainer (#5953):

- Re-arm the probe timer on receipt from the peer while probes are
  outstanding. Don't cap the backoff exponent: RFC 9002 §6.2.1 requires it
  to double, and an opt-in cap would be a knob for one deployment.
- Declined from #5953: a congestion controller that holds a fixed rate
  through random loss, exposing the reorder thresholds, and a named
  provisioned-path profile. Random loss is not a quality target
  ([random-loss audit](/quest/m1/random-loss-audit.md)), and the reorder
  evidence came from random netem reorder only. Revisit the thresholds with
  a bufferbloat or real-path measurement.
- Test on mocked time: a link that drops everything for 5 s, then returns.
  Assert the first post-outage send follows the peer's first packet within
  one RTT, and that a sender hearing nothing still backs off.
- `MAX_BACKOFF_EXPONENT` lives in `rs/moq-quic/src/connection/mod.rs`.

Public API: none.

## Closes

- [#5953](https://github.com/moq-dev/moq/issues/5953) - QUIC settings for provisioned contribution paths

## Related

- [Keep-alive from the idle timeout](/quest/m2/quic-keep-alive.md) - the other outage-time knob
- [Per-stream deadlines](/quest/m2/quic-deadline.md) - an early probe ahead of a deadline
- [Restart reset](/quest/m1/stateless-reset-key.md) - the restart half of #5953's item 4
