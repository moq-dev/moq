# [M] A subscription from an old relay mid-roll is fed

## Goal

Mid-roll, a new relay feeds every SUBSCRIBE an old (lite-06) neighbour sends
it, including one that arrives as that neighbour withdraws its own echo of the
broadcast. Today the new relay accepts the subscription and forwards no group
until the broadcast ends, so the old relay's viewers stay subscribed, see the
broadcast live, and read nothing.

No reader starves in
`just test mesh-replay --mesh test/mesh-replay/chain3.toml --script test/mesh-replay/half-roll.toml`
or in the nightly ring meshes' rolls.

## Plan

Found by [the mesh replay](/test/mesh-replay/README.md): in the nightly ring of
six, rolling `d` starved reader `e` on two of `c`'s broadcasts, and rolling `e`
starved reader `f` on all four of `d`'s. The chain reproduction starved a
reader in 3 of 5 runs; one other ended in [the detour stall](/quest/m2/multicast/detour-resume.md).

What the logs of the chain (`a` dials `b` dials `c`, roll `a` then `b`) show,
with `b` the new relay and `c` the old one:

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
`rs/moq-net/src/lite/publisher.rs` and `rs/moq-net/src/model/`. The kept run
directories of the reproduction (`HARNESS_KEEP=1`, `MESH_REPLAY_LOG=debug`)
have both relays' logs.

It needs an old relay, so it lasts only as long as a roll does, but every roll
passes through it, once per relay, and a starved viewer sees a live broadcast
that never plays. Recommended: under this epic, so the lite-07 cut waits on it,
since the cut ships by roll. Whether the cut waits is the maintainer's call.

Not a wire change.
