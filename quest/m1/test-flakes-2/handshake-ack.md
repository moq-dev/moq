# [S] A lost handshake ACK does not stall an impaired connect

## Goal

The impaired cluster drill connects its publisher and subscriber however the
shaper drops their handshake ACKs. `MOQ_SHAPER_SEED=13876846873975217772`
`bursts_cross_a_cluster::impaired` (`rs/moq-relay/tests/drills.rs`), which
fails on `main` every run with `publisher connect timed out`, passes.

## Plan

Reproduced 2026-10-13 on `0e3ca5e6b`, with noq trace logs. It fails 3 of 3
runs at that seed. 200 random seeds all passed, so this is a rare tail, and
the merge queue hit it twice that day.

The path makes every handshake flight two full datagrams that cannot cross
together. The hybrid X25519MLKEM768 key share makes the ClientHello 1491
bytes and the ServerHello 1178, so each side's first flight is 2 x 1200
bytes. `bursty()`'s bucket admits 1500 bytes of burst plus a 50 ms queue at
100 kbit/s, 2125 bytes in all. The second datagram of every back-to-back
flight and every two-packet PTO probe overflows, every time. A connect
through it only progresses when an ACK tells the sender which half arrived,
so the sender can retransmit just the other half.

The failing seed: the client receives the server's first Initial (the
ServerHello head) and ACKs it three times, at +0.2s, +1.2s, and +4.2s. The
shaper's 5% loss drops all three. Between them the client sends Initial
probes that reach the server, but noq writes ACK frames only when
`PendingAcks::can_send()` holds. That is false once an ACK was sent and no
new packet arrived, so the probes carry no ACK. The server keeps resending
head and tail together, the tail always overflows, and the client never
gets Handshake keys. The fourth ACK lands at +9.2s. The server then fires
its PTO six times in one instant and bursts 12 Handshake probes, and the
drill's 10s connect budget runs out first.

This is not a budget problem. Patching noq to repeat pending Initial and
Handshake ACK ranges in any packet it sends in that space makes the seed
pass, with the whole drill done in 6-7s (3 of 3 runs). 200 random seeds
still pass with it. A longer `TIMEOUT` would only move the tail, since
nothing repeats a lost ACK until the peer's next backed-off PTO.

`moq-quic` already has the fix: quinn bundles pending ACK ranges into
packets that carry other frames (`can_send_with_other_frames`, paced by
`next_bundled_ack_time`). noq predates it.

Decision (maintainer, 2026-10-13; ✅ marks the choice):

- ✅ Port it to the frozen moq-dev/noq: repeat pending Initial and Handshake
  ACK ranges in any packet sent in that space, as `moq-quic`'s
  `can_send_with_other_frames` does, then bump the `moq-noq-proto` pin.
  [#32](https://github.com/moq-dev/noq/pull/32) and
  [#34](https://github.com/moq-dev/noq/pull/34) made the same exception for
  this drill's last handshake flake.
- Wait for [the switch](/quest/m1/quic/fork/switch.md) to `moq-quic`, which
  has not started.

Land a mock-time regression test in noq with the fix: the peer's first ACK
of a two-datagram Initial flight is lost, and the next packet in that space
carries the ACK. Check that `moq-quic` passes the
same test, so the switch keeps the behavior.

Also look at the PTO cascade. When the ACK reset `pto_count`, the Handshake
PTO deadline was already in the past, so the timer fired back to back until
the backoff passed `now`. Each firing queued two probes. Fix it only if
`moq-quic` does the same.

Not covered: the `subscription aborted: internal error` failure also seen in
the merge queue. No seed was recorded for it, so it may be a different cause.

Public API: none in this repo. Wire: none.

## Related

- [Switch](/quest/m1/quic/fork/switch.md) - moves the runtimes onto `moq-quic`, which already bundles ACKs
