# [XS] iroh's noq carries quinn's stream reassembly cap

## Goal

The upstream `noq-proto` that the relay's default `iroh` feature pulls in
bounds how many out-of-order chunks a stream or CRYPTO buffer holds. Today it
is 1.3.0, which predates quinn's fix (RUSTSEC-2026-0185), and `cargo audit`
cannot match it because the crate is renamed.

This waits on n0: check whether
[n0-computer/noq#828](https://github.com/n0-computer/noq/pull/828) is merged
and released, then bump `noq` / `noq-proto` (directly or through `iroh`) on
`main` and `release`.

## Plan

`moq-noq` already carries the cap (2.0.1 on `main`, 1.3.3 on `release`), and
`moq-tokio` defaults the connection receive window to 64 MiB on noq and iroh,
so only this pin remains.

## Related

- [Peer limits](/quest/m1/quic/peer-limits.md) - per-peer windows and stream limits, which later let cluster sessions take a larger window than clients
