# [M] Characterize moq-quic behavior

## Goal

Tests on today's harness pin the behavior the QUIC rewrites touch, so a
rebase onto any of them fails loudly wherever behavior changed.

- **Scheduling**: golden tests of the exact order today's scheduler sends
  stream bytes across priorities, equal-priority streams, retransmits, and
  datagrams interleaved with streams. #5711 changes this order on purpose
  and must update each golden in its own diff.
- **Frame error paths**: CRYPTO in 0-RTT, STREAM_DATA_BLOCKED or
  MAX_STREAM_DATA on the wrong direction or an unopened stream, STOP_SENDING
  on a receive-only or unopened stream, STREAMS_BLOCKED and MAX_STREAMS
  above the limit (bidi included), NEW_CONNECTION_ID limit and retirement
  errors, NEW_TOKEN and HANDSHAKE_DONE from a client, offsets at 2^62, and
  final-size errors on data and reset.
- **Resets**: `RecvStream::received_reset`, reset codes, `Chunks` ending in
  Reset or Finished, and connection credit returned on reset, stop, and
  0-RTT rejection.
- **Runtime limits**: `set_max_concurrent_streams`, `set_receive_window`,
  and `set_send_window` raised and lowered mid-connection, including the
  remote-bidi window raise in `set_params`.
- **Endpoint**: unknown and invalid CIDs, long headers for unknown
  connections, short, undecodable, and wrong-version initials, the
  `cids_exhausted` refusal, and a stateless reset from a client-only
  endpoint.
- **Packets and paths**: the AEAD integrity limit, duplicate discard,
  invalid Retry handling, 0-RTT retransmit after Retry, path-validation
  failure reverting the path, and IPv4 rebinding through
  `PathData::from_previous`.

## Plan

Decided 2026-10-16: the scheduling tests pin the exact old order rather
than only invariants, so every intentional change in #5711 is visible and
reviewed. If #5711 merges first, write the goldens against the baseline
(b30289127) anyway and update them on rebase, so the PR still shows the
diff.

The survey's line numbers for each uncovered path come from
`cargo llvm-cov` on the baseline; [the coverage report](/quest/m1/quic/coverage/report.md)
reproduces them. This quest uses today's `Pair` as is; seeded impairments
belong to [the simulation](/quest/m1/quic/coverage/sim.md).
