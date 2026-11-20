# [M] A reader refused as unroutable reads again on lite-07

## Goal

On lite-07, a reader whose relay still shows its broadcast keeps reading after
a link reopens or a relay crashes, as the release (lite-06) does. Today, about
one run in twenty, a relay answers a reader's live subscription with
Unroutable while it keeps announcing the broadcast unchanged. The reader's
track strands, waiting for a route change that never comes, and starves for
the rest of the run without an error.

No reader starves in
`just test mesh-replay --mesh test/mesh-replay/ring4.toml --script test/mesh-replay/reopen.toml --broadcasts 32`
or in the nightly ring meshes, once `[[known]]` no longer names this quest in
`test/mesh-replay/script.toml`.

## Plan

Found by [the mesh replay](/test/mesh-replay/README.md). On main after
[roll-echo-stall](https://github.com/moq-dev/moq/pull/9174) and
[reopen-starve](https://github.com/moq-dev/moq/pull/9179), 19 of 20
`reopen.toml` runs (ring4, 32 broadcasts, debug build) passed. In the failing
run, `b`'s reader followed `d/0` and `d/3` through `a`. When the `a`-`b`
session idle-timed out, relay `b` reset the reader's live subscriptions for
exactly those two paths with Unroutable, and never withdrew or updated `d/0`.
The reader's front strands a track refused as unroutable until its table moves
(`model/front.rs` `refuse`, `Track::stranded`, `unstrand`), so it never asked
again. The other 30 paths switched to the copy through `c`. That run had info
logs only.

Decided 2026-11-20 by the maintainer:

- ✅ The hop holds (rejected: the reader re-asks on a timer, which adds a
  backstop timer and can loop on a refusal that stands). A hop never answers a
  live subscription with Unroutable while it still shows that broadcast
  available. Its front either waits for a route, as it already does for an
  orphaned claim (`Front::waiting`), or changes its announcement so the reader
  asks again.
- ✅ The lite-07 cut waits on this fix.

What [moq-dev/moq#9219](https://github.com/moq-dev/moq/pull/9219) did:

- Closed one way a hop broke that rule. A lite session fails the copies it
  carries before it closes the broadcasts they came from. A front that saw a
  copy die with its session's end (`IdleTimeout`, `Session`, `Transport`) from a
  source whose broadcast still read as open asked that source again and took
  its answer as the track's verdict. Relayed, that end reads as Unroutable.
  The front now counts a received serving source as closing on such an error
  (`Front::closing`, `Error::session_ended`) and holds the track for the next
  route. `/verify-failover` models the window: a dead relay's route still
  shows until it leaves the table (`Shows`, `Withdraw`).
- Added a debug line for every front step (`moq_net::model`), so a failing run
  shows how a front ended a track.

That race was not confirmed as this failure. On an unloaded host, 59 base runs
and 24 fixed runs passed, and no reader got Unroutable during the single link
reopen in any of them. Holding the drain open 2s after the transport closed
did not make the base fail either. Traced relays show the source closing
before its copies end. Debug runs on the CI host at load 70 or more starve
readers by load alone: sessions to the detours idle-time out too.

Next steps: capture one failure with
`RUST_LOG=info,moq_net::model=debug`, on a host under real but not saturating
load (relays below a core each), and record per-relay CPU with it. Rerun
`reopen.toml` on a debug build until a reader gets Unroutable during the
single link reopen; that alone marks the bug, starving or not. In the failing
relay's log, find the front step that ended that path. Then write the test at
the lowest layer that shows it, fix it as decided, rerun `/verify-failover`,
and rerun the replay at least 20 times before dropping the `[[known]]` entries
in `test/mesh-replay/script.toml`.

Pauses are not part of this quest. A debug build pauses about as long on
lite-06 as on lite-07 under the same load, and a release build the same on
both versions.

Not a wire change.

## Related

- [reopen-starve](https://github.com/moq-dev/moq/pull/9179) - a request
  refused before the neighbour's claims returned, and never asked again
- [roll-echo-stall](https://github.com/moq-dev/moq/pull/9174) - a time start a
  shared copy never places
