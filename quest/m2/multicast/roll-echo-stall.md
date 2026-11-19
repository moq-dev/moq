# [M] A subscription from an old relay mid-roll is fed

## Goal

Mid-roll, a new relay feeds every SUBSCRIBE an old (lite-06) neighbour sends
it, including one that arrives as that neighbour withdraws its own echo of the
broadcast, and a front left from the roll resumes after the next route change.
Today the new relay accepts the subscription and forwards no group until the
broadcast ends, so the old relay's viewers stay subscribed, see the broadcast
live, and read nothing.

No reader starves in
`just test mesh-replay --mesh test/mesh-replay/ring6.toml --script test/mesh-replay/half-roll.toml --broadcasts 32`
or in the nightly ring meshes, once `[[known]]` no longer names this quest in
`test/mesh-replay/script.toml`.

## Plan

Found by [the mesh replay](/test/mesh-replay/README.md). In the ring of six,
rolling `b` onto the new build starved old relay `c`'s readers of `a`'s
broadcasts in 3 of 4 nightly cells: 9 of 24 paths at 4 broadcasts per relay,
40 and 41 of 192 at 32. Rolls further on starved others the same way. The half
roll alone (`a` and `b`) starved them in 1 of 3 runs at 4 broadcasts.

What the logs of a chain of three show (`a` dials `b` dials `c`, roll `a` then
`b`), with `b` the new relay and `c` the old one:

1. `b` restarts on the new build and dials `c`. Its predecessor's session to `c`
   is still draining, so the old relay `c` announces `a`'s broadcasts to the new
   `b` over lite-06 with three hops, learned through that predecessor: `b`
   hears its own stale state echoed back before its link to `a` is up.
2. `b`'s link to `a` comes up a second later. `b` pulls some of `a`'s broadcasts
   from `a` for its own reader and for `c`'s first subscribes, and those flow.
3. The predecessor's session to `c` times out (the 10s idle timeout). In one
   millisecond, `c` unannounces the echoed broadcasts to `b` and resubscribes
   the ones it read through the predecessor, now on the new `b`.
4. `b` logs `subscribed started` for those subscribes and serves nothing until
   the publisher stops 50s later, when it serves one group from step 3 and
   then the final group. Its own upstream subscription to `a` for the same
   broadcasts is live the whole time, feeding `b`'s reader.

So a lite-06 subscription that resolves while its broadcast's echoed route is
being withdrawn attaches to something that never delivers, and nothing moves
it to the live route. Likely in how the publisher resolves a request against
the origin while a route is withdrawn: the wait for a claim's route
([moq-dev/moq#9052](https://github.com/moq-dev/moq/pull/9052)), or the lite-06
path's announce handling (hop counts are not checked against loops), in
`rs/moq-net/src/lite/publisher.rs` and `rs/moq-net/src/model/`. Rerun after
[moq-dev/moq#9112](https://github.com/moq-dev/moq/pull/9112), which changes how
a let-go claim's copies end but keeps lite-06 routes as they are. Run with
`HARNESS_KEEP=1 MESH_REPLAY_LOG=debug` to keep every relay's debug log.

A residue on lite-07: after the whole ring rolled onto the new build, a cut of
the `a`-`b` link left a reader on `a` of one of `b`'s broadcasts online with no
group for 75s, resumed neither on the detour through `f` nor on the redialed
session to `b`, until the next fault moved its route (1 path at 4 broadcasts;
5 at 32, and 33 after reopening every link of `b`). Ten cuts of the same link on
a ring that started on the new build starved nothing, so it takes the roll
first. Check it once the mid-roll case is fixed.

It needs an old relay, so it lasts only as long as a roll does, but every roll
passes through it, once per relay, and a starved viewer sees a live broadcast
that never plays. Recommended: under this epic, so the lite-07 cut waits on it,
since the cut ships by roll. Whether the cut waits is the maintainer's call.

Not a wire change.
