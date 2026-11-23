# [S] Port recent quinn-rs/quinn fixes into moq-quic before 0.22

## Goal

Every applicable correctness fix from `quinn-rs/quinn` since `aea1e538` (2026-11-03) is ported into `rs/moq-quic` (or recorded as not applicable) with regression tests, and `rs/moq-quic/README.md` records the new checked commit before 0.22 cuts.

## Plan

Found by the 2026-11-23 pre-0.22 dependency audit (`890ea4e531`). `rs/moq-quic/README.md` was last synced through `quinn-rs/quinn` `main` at `aea1e538` (2026-11-03). Zero new GHSA security advisories were published since 2026-09-30, but six non-dependency fixes landed upstream since `aea1e538`:

- `aa61ce996b` (2026-11-01): `proto: don't count late ACK-only packets as spurious losses`
- `765abecf7b` (2026-11-04): `Avoid poisoned-lock panics during cleanup`
- `dadde9a681` (2026-11-06) and `8fe66c31d7` (2026-10-26): `quinn: release sender on connection driver drop` / `release cancelled stopped notification registrations`
- `26711b2e1b` (2026-11-09): `proto: wait for the protocol before server handshake data after a retry`
- `ca3566ce19` (2026-11-09): `proto: discard incoming 0-RTT packets as client`
- `0f9432f7c1` (2026-11-13): `quinn-proto: Decode a run of PADDING bytes as one frame`

Check each commit against `rs/moq-quic` (and `rs/moq-tokio`'s QUIC driver) to verify whether the fork already holds the fix or needs it ported, port each applicable fix with its upstream regression test, and update `rs/moq-quic/README.md` (and note the new checked head in [vendor-sync/quinn](/quest/m3/vendor-sync/quinn.md)).

Public API: none. Wire: QUIC protocol correctness only.
