# [S] The moq-net session bench completes moq-transport-22 at 16 broadcasts

## Goal

Nightly's "Rust bench targets" workload runs `moq-net`'s session bench to the
end: `session_delivery_broadcasts/moq-transport-22/.../broadcasts=16` no
longer panics with `server handshake failed: Timeout`
(`harness.rs:233`).

## Plan

Planned 2026-11-09 (maintainer, /quest-complete from the triage of Nightly
run [42487375545](https://github.com/moq-dev/moq/actions/runs/42487375545),
where it first failed this way). Reproduce it first, then fix the cause:
a handshake that stalls under that load is a bug in the session or the
harness, never a reason to raise the timeout. Check whether the harness
reuses state across cells and whether lite versions pass the same cell.

Public API: none expected. Wire: none.
