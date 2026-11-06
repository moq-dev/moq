# [S] The QUIC driver tells an MTU probe from an oversized send

## Goal

moq-tokio's QUIC driver (`send_failed` in
`rs/moq-tokio/src/quic/driver/runtime/mod.rs`) still drops an `EMSGSIZE` from
an MTU probe quietly, but a real oversized send that fails with `EMSGSIZE` is
logged and counted, so it can't be dropped silently again.

## Plan

Decided 2026-11-03 (maintainer, follow-up planning after the 2026-11-03 /quest-afk and /quest-complete runs), milestone:

- m0
- m1
- ✅ m2

From [moq-dev/moq#7649](https://github.com/moq-dev/moq/pull/7649), which
capped a GSO train at what one UDP send carries after full trains above a
~6.5 KB MTU failed with `EMSGSIZE` and were dropped unlogged: `send_failed`
treats every Unix `EMSGSIZE` as an expected MTU-probe loss.

- Tell the two apart from the transmit itself (a probe's size against the
  path's confirmed MTU, or a probe flag the transmit carries).
- Log a non-probe `EMSGSIZE` (rate-limited, like the driver's other send
  errors) and count it where the driver's metrics live.
- Apply the same rule in moq-uring's driver if it has the same blanket case.
- Test both cases with a socket that fails with `EMSGSIZE`.

Public API: none expected. Wire: none.
