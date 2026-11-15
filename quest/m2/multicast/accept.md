# [L] The cases no test covers run on real sessions

## Goal

On real lite-07 sessions with mocked time, the switched route layer passes
the cases the 2026-11-15 audit found neither a model nor a test for, and the
PR reports the measured interruption for each kind of failure:

- two publishers claiming one path from opposite sides of a mesh, where
  each relay uses its nearest and none loops;
- one publisher ingesting on two relays under one Instance, losing either,
  with readers across the mesh reading on, and a pair under two Instances,
  where the move is a restart and nothing is spliced;
- a publisher handed over by GOAWAY to another relay, with no group missed
  or repeated;
- moq-transport and lite-06 publishers through the route layer, including
  two announcing one namespace on different relays, each a minted Instance
  that no reader is moved between mid-stream;
- three meshes joined by scoped links, in a chain and in a ring, where no
  claim circles and a claim ended at its source ends everywhere;
- an edge that marks its core links and a leaf that marks every link, each
  passing only what requirement 3 allows;
- a relay that dies silently while its neighbours notice seconds apart;
- failures at the same instant on a mesh no exhaustive check reaches;
- and no group crossing one link twice for one subscription.

## Plan

Decided 2026-11-15 (maintainer, /quest-plan): the redesign is accepted on
the implementation before lite-07 freezes, as the maintainer decided for the
path rule on 2026-11-14.

[Route layer acceptance](/quest/m2/cluster-routes/acceptance.md) owns the
fixtures that exist, which the [switch](/quest/m2/multicast/switch.md)
rewrites for the new rule; this adds the cases above beside them in
`rs/moq-net/tests/integration/`. Impair links with bufferbloat, not random
loss. Where a case needs `js/net`, run it in `just test interop --all`.

A case that fails is a flaw to fix on lite-07-wip, in the quest that owns
the code, before the cut.

Public API: none. Wire: none expected.

## Required

- [The switch](/quest/m2/multicast/switch.md) - the sessions these cases run on
- [Requests](/quest/m2/multicast/requests.md) - a request fails closed and follows its Instance
- [The proof](/quest/m2/multicast/proof.md) - what these cases need not show
