# [M] A reader reads again after a lite-07 link reopens

## Goal

On lite-07, a reader behind a link that reopened or a relay that crashed reads
again once a route to its broadcast returns, as the release (lite-06) does
within seconds. Today a reader two hops away can show the broadcast live and
read nothing for 75s or more, until some later fault moves its route.

No reader starves in
`just test mesh-replay --mesh test/mesh-replay/ring4.toml --script test/mesh-replay/reopen.toml --broadcasts 32`
or in the nightly ring meshes, once `[[known]]` no longer names this quest in
`test/mesh-replay/script.toml`.

## Plan

Found by [the mesh replay](/test/mesh-replay/README.md), at 32 broadcasts per
relay:

- ring4 with every relay on the new build and no roll (`reopen.toml`): reader
  `c` starved on all 32 of `a`'s broadcasts for 75s after `a`'s links reopened,
  and a second run starved readers in both reopens and the kill. The same faults
  with every relay on the release, under the same load, fed every reader again
  within 4.1s.
- ring6 nightly, after the forward roll: a cut of `a`-`b` starved 186 reader
  paths, the kill of `b` 269, and one path stayed starved through four later
  events.
- At 4 broadcasts it is rare: 1 path after a cut and 1 after a kill in one of
  three ring6 runs, and none in ten cuts of a ring6 that started on the new
  build.

No shaper dropped or overflowed a packet, and each reader's once-a-second
status kept time while its paths starved, so it is not the host's load.

What the relay logs show: the reader's relay subscribed through a neighbour
whose own route had not returned yet, the neighbour refused it as unroutable,
and the relay never asked again, though the neighbour's route came back
seconds later. The reader's follow stays online with no group, so nothing
above it retries either.

Likely cause: a track refused as unroutable is `stranded` on its source and
waits "for another source, or for the table to move" (`rs/moq-net/src/model/front.rs`,
`Track::stranded`, and `unstrand`, which asks again only when the serving
source changes). When the neighbour's route returns behind the same source,
this relay's table does not move, so the track is never asked for again. A fix
re-asks a stranded track when its source's route to the broadcast is shown
again, without asking a source that still refuses in a loop. Rerun the
`verify-failover` model, since this changes the front lifecycle it covers.

Decided 2026-11-19 (under the maintainer's delegation for lite-07 routing
calls while away): ✅ the lite-07 cut waits on it (rejected: cutting without
it, or deferring it to after the cut). lite-06 settles the same faults in 4s,
so it is a lite-07 regression on a link flap or a crash, which a deployment
sees daily.

Not a wire change.
