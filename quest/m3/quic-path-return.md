# [M] A path that's back is used within an RTT

## Goal

After a link outage in either direction, `moq-quic` resumes sending within
about one RTT of the path's return, and the outage doesn't leave smoothed RTT
inflated. Today a 5 s one-way outage leaves the relay's next send 2.4 s after
the link returns, on its backed-off probe timer. A two-way outage raises
smoothed RTT from 105 ms to between 0.74 and 0.99 s, and puts the next send
0.95 to 7.76 s after the return. The consumer is MPEG-TS trunks under a fixed
`moq export ts --delay`, which compete with SRT on outage loss.

## Plan

Decided 2026-11-12 (maintainer, `/quest-import` of
[moq-dev/moq#8559](https://github.com/moq-dev/moq/issues/8559)):
✅ plan this recovery quest in m3 and restore
[Per-stream and per-datagram deadlines](/quest/m4/quic-deadline.md) in m4
(rejected: recovery only; nothing on QUIC until the order fix is measured).
`quic-careful-resume` stays deleted until a redial's ramp is measured; the
issue reasons it but has not isolated it.

Verified in `moq-quic`'s connection code:

- One-way outage. `on_loss_detection_timeout` arms `probe_when_heard` once
  ([moq-dev/moq#6178](https://github.com/moq-dev/moq/pull/6178)), and
  `probe_on_heard` clears it on the first packet heard. In a one-way outage,
  the subscriber's own packets spend it while the relay's direction is still
  dead, so nothing remains to fire when that direction returns.
  `pto_time_and_space` then waits out `pto_base * 2^pto_count`.
- Two-way outage. `RttEstimator::update` takes the first acknowledgment after
  the return as an ordinary sample, which spans the outage (about 5 s), with
  no outlier guard.

Recommended fixes, to be confirmed by measurement:

- Keep the heard-probe armed until new data is acknowledged, rate-limited so
  a one-way outage doesn't send a probe per packet received (#6178's
  concern).
- Keep a sample out of smoothed RTT when its packet was sent before a probe
  timeout that went unanswered. RFC 9002 leaves this to the implementation.

Grade on the issue's shape (20 Mb/s `cake`, 100 ms RTT, a 9.95 Mb/s
multiplex, 5 s outage one-way and both ways, three replicates) and on
bufferbloat: time from the return to the first send and to the multiplex's
rate, and video lost under `--delay` 2 s, 5.75 s, and 11.71 s. Report
spurious probes during the outage.

Public API: none. Wire: none.

## Closes

- [#8559](https://github.com/moq-dev/moq/issues/8559) - a 5 s outage costs a trunk about the outage, whatever the delay

## Related

- [Buffered exports pick their group order by measurement](/quest/m3/export-order.md) - the send-order half of the same outage
- [Per-stream and per-datagram deadlines](/quest/m4/quic-deadline.md) - faster recovery below a deadline
