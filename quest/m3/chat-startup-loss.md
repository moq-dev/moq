# [M] The relay's chat startup loss is explained

## Goal

The groups `main`'s relay loses while `chat`'s 2,000 viewers join have a named
cause: a real delivery loss, fixed or accepted with what it pays for, or
groups now reported dropped that `release` never reported, documented in
`.github/release-perf.md` beside the relay CPU finding.

## Plan

Found by [Relay CPU](https://github.com/moq-dev/moq/pull/8979)
([release-perf.md](/.github/release-perf.md#relay-cpu-2026-11-16)).

Facts:

- The relay memory pairing saw `chat`'s lifetime `loss_pct` at about 1.5% on
  `main` against 0.1% on `release`, all during the startup ramp:
  `groups_lost` stops growing once the load is ready.
- In the relay CPU runs (`just bench-relay origin/release quic`, `fedora`,
  load 24 to 66), three of sixteen `main` `chat` rounds, with and without its
  fix, lost 6.7% to 11.9% of groups and still delivered above the offered
  20,000 frames/s in the measured window, draining a backlog. The other
  thirteen lost 0.04% to 1.8%; `release`'s six lost at most 0.06%. The lossy
  rounds read the highest relay CPU per frame.

Start from one lossy round's `load.jsonl`: whether the lost groups are drops
the relay reported to viewers that joined after them (lite-05+ drop
forwarding, which `release` lacks) or groups a viewer waited for and never
got. The generator's per-connection reports say which viewers lost them and
when.

## Related

- [Track lock passes](/quest/m3/relay-track-lock.md) - the same subscribers' steady-state CPU
