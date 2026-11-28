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

Update 2026-11-28 (`/quest-start`): [`half-roll-hold.toml`](/test/mesh-replay/half-roll-hold.toml)
(relays `a` and `b` rolled so `a-b` is the only lite-07 link, then a 10-minute `hold` that fails on
any reader pause past `stall`) reproduces it deterministically on one loaded macOS host, and
[`half-roll.toml`](/test/mesh-replay/half-roll.toml) alone does at its first roll. Readers on `a`
(then `b`) of other relays' broadcasts read nothing from the roll on, and never recovered within
the 13-minute run (no 1 to 3 minute recovery). With the relay built at each commit and this
checkout's `moq-bench`, 4 broadcasts per relay, one run each:

| Relay build | Result |
| --- | --- |
| `5d86f05321` | starved, 791s pause in the hold |
| `a1c13dd38d` (parent of #9564) | starved |
| `64c69c2548` ([#9564](https://github.com/moq-dev/moq/pull/9564), its S5 merged) | starved (`half-roll.toml`) |
| `f24ac5065e` (`main`) | clean: `half-roll.toml` once, `half-roll-hold.toml` at 4 and 32 broadcasts |

So #9564 is not the fix, and something in the 42 net, relay, and quic commits between
`64c69c2548` and `f24ac5065e` is. Next: bisect them with `half-roll.toml` (about 3 minutes a
run, 6 builds), name the commit, check it has a test at the lowest layer that fails without it,
and add one if not. If that commit is a quic or cc change (#9709 to #9716, #9745) the stall may
be a send-window starvation the lite code only exposed. Not yet checked: that these starved
readers are the quest's 1 to 3 minute stall rather than another mid-roll strand.

If the fix needs a change to the lite-07-wip wire, that part joins m2 under
[lite-07 subscribe](/quest/m2/lite07-subscribe/README.md), per the milestone
rules in [the questline](/quest/README.md), and this quest keeps any non-wire
part.

Decided 2026-11-25 (maintainer, `/quest-plan`): ✅ keep (rejected: drop).

## Related

- [lite-07 subscribe](/quest/m2/lite07-subscribe/README.md) - the lite-07 subscribe redesign, where a wire change for this would go
- [Route health](/quest/m3/route-health/README.md) - failover off a stalled route, which would mask rather than fix this
- [Nightly](/quest/m0/nightly/README.md) - its notes on earlier mesh replay violations, some from CPU-starved relays
