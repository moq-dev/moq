# [M] Readers stay fed when two subscribers share a mesh's only lite-07 link

## Goal

Readers whose route crosses a mesh's only moq-lite-07 link keep reading while a
second subscriber shares that link, fixed at its cause in the lite subscriber or
route code. Today, in the mesh replay (`test/mesh-replay/`), while two
subscribers shared the mesh's only lite-07 link, the readers on one relay whose
route crossed another relay slowed, then read nothing for 1 to 3 minutes, and
recovered without a restart. Done when the replay that reproduced it runs clean
several times in a row, and a test at the lowest layer that fails without the fix
holds it.

## Plan

Evidence: it reproduced in 3 runs, at upstream commits `5d86f05321`,
`fd59727e35`, and the `release` branch's `b9adb99467`, and not in a run at
`e56d723e60` or in two short runs. `e56d723e60` sits between two commits that
reproduced it, so this is intermittent rather than a regression in that range,
and long runs are what reproduce it; don't bisect across it.

1. Reproduce it in the replay first: a mid-roll mesh where two relays on the new
   build share the only lite-07 link, as
   [`half-roll.toml`](/test/mesh-replay/half-roll.toml) leaves one, with two
   readers whose routes cross it, run long enough to show the stall. Keep its
   reader and relay logs (`HARNESS_KEEP=1`) and record the shape and rate here.
   If it needs a mesh or script the tree lacks, add the smallest one that
   reproduces it.
2. From the logs, find where the stalled readers' groups stop: the subscriber's
   requests on the shared link (`rs/moq-net/src/lite/subscriber.rs`), the route
   layer's choice for them (`rs/moq-net/src/route/`), or flow control on the link.
   A 1 to 3 minute recovery with no restart points at something that clears on a
   timer or on unrelated traffic, so name what ended each stall.
3. Check the bugs [moq-dev/moq#9564](https://github.com/moq-dev/moq/pull/9564)
   fixed before writing a fix: its S5 (a Subscribe Stream's serve loop that stops
   reading while it flushes) starves exactly this way when two subscriptions fill
   one link's send windows. If one of its bugs is the cause, fix that bug here or
   land that PR first, and keep this quest's replay check either way.
4. Fix the cause, and add the regression test at the lowest layer that fails on
   `main` (a `moq-net` integration test on mocked time where it reproduces there).

Route-health failover would move a stalled track to another route, but it would
hide this stall, not fix it, and the mesh's only lite-07 link has no lite-07
alternative.

Update 2026-11-28 (`/quest-start`): it did not reproduce in the replay. The new
[`half-roll-hold.toml`](/test/mesh-replay/half-roll-hold.toml) (relays `a` and `b` rolled so `a-b`
is the only lite-07 link, then a 10-minute `hold` that fails on any reader pause past `stall`)
ran clean, 4 broadcasts per relay, on a loaded macOS host at:

- `5d86f05321` built whole (relay, `moq-bench`, and shaper from that commit, the replay harness
  copied over it), one run;
- `main` at `f24ac5065e`, 4 and 32 broadcasts, two runs.

The original three runs' mesh, broadcast count, and logs are unknown, and Nightly's mesh replays
also passed at `5d86f05321`. A relay built at an older commit run against this checkout's
`moq-bench` starves every reader from the first roll, but that is skew, not this bug: the client
sends lite-07's `Meta` byte (added in `eed4c1850c`) and the older relay answers PROTOCOL_VIOLATION.
Build both from one commit when replaying an old build.

What remains is to catch it again: run the hold on the nightly's hosts (Linux, 32 broadcasts)
a few nights with `HARNESS_KEEP=1`, and if it still never fails, delete this quest.

If the fix needs a change to the lite-07-wip wire, that part joins m2 under
[lite-07 subscribe](/quest/m2/lite07-subscribe/README.md), per the milestone
rules in [the questline](/quest/README.md), and this quest keeps any non-wire
part.

Decided 2026-11-25 (maintainer, `/quest-plan`): ✅ keep (rejected: drop).

## Waiting

- Maintainer: the stall did not reproduce at `5d86f05321` built whole or on `main`; keep the quest?
  - Delete it, keeping the hold check (Recommended) - nothing reproduces it, and the hold soaks that exact state; reopen from the check's failure with logs if it ever fires
  - Keep it for a few nightly hold runs on Linux - costs a nightly job's 10 minutes a cell to rule out a host-specific cause

## Related

- [lite-07 subscribe](/quest/m2/lite07-subscribe/README.md) - the lite-07 subscribe redesign, where a wire change for this would go
- [Route health](/quest/m3/route-health/README.md) - failover off a stalled route, which would mask rather than fix this
- [Nightly](/quest/m0/nightly/README.md) - its notes on earlier mesh replay violations, some from CPU-starved relays
