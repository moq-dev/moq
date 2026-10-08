# [S] Impaired drill clients connect reliably

## Goal

The impaired cluster drills (`bursts_cross_a_cluster::impaired`,
`bursts_cross_a_flapping_peer::impaired` in `rs/moq-relay/tests/drills.rs`)
never fail while connecting their publisher or subscriber.

## Plan

Measured 2026-10-07 by running the built drill binary in a loop (four tests
in parallel):

- `main` at `fe0113ff2`: 1 of 40 runs failed `publisher connect failed:
  Noq(Connection("timed out"))`, seed 3495212678159036442.
- #4972, with both relays back on the default 10s idle timeout: 3 of 40 runs,
  `subscriber connect failed: Noq(Connection("timed out"))` twice and once
  `failed to exchange h3 settings: ... reset by peer`.

So the relays' idle timeout is not the cause. The likely cause is the drills'
2s client idle timeout (`quic()`) against a handshake over 5% loss and a
100 kbit/s bottleneck: noq arms the idle timer at `max(idle, 3 * PTO)` with
the base PTO, while a backed-off PTO can leave the client silent longer than
that. Confirm with a qlog of a failing seed, then fix it at the cause, without
raising a timeout or retrying the connect.

Public API: none. Wire: none.
