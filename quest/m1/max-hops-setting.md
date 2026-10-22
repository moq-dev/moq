# [S] A relay setting bounds path length

## Goal

A relay operator can set the longest hop path a relay accepts or forwards,
with today's 32 as the default. On a mesh whose diameter is a few hops, a
cap near diameter plus a margin stops lite-06 path hunting from exploring
long detours after a withdrawal. The setting is backported to `release`.

## Plan

Found in the 2026-10-22 clustering audit: `MAX_HOPS` is 32 in
`rs/moq-net/src/model/origin.rs` on both `main` and `release`, enforced on
receive in `lite/subscriber.rs` and on send in `lite/publisher.rs`. On a
diameter-4 mesh, a withdrawn route can be re-advertised over paths up to 32
hops long before anything drops it, which shows up as periodic announce
storms in dense meshes.

- Expose the cap as a relay cluster setting (CLI, env, and TOML like the other
  cluster settings), default 32, refusing 0 and values above 32. A
  lower cap only drops longer paths; it never changes which route wins among
  the shorter ones.
- The cap applies to hop-list versions (lite-06 and earlier, and lite-07-wip
  until [Routes and announces](/quest/m1/cluster-routing/routes.md) removes
  the hop list). Once routes land, lite-07 ignores it; document that.
- Operators size it to their topology's diameter after any single link or
  node failure plus margin. The doc says so and warns that a cap below that
  partitions the mesh.

Tests: `mesh_withdraw` sweeps run with a cap near diameter plus two and
record fewer path-hunting observations than the default, without changing
the residual counterexamples' final state. A path longer than the cap is
dropped on receive and never sent.

Docs: `doc/bin/relay/cluster.md` gains the setting. Public API: one relay
config field. Wire: none. Backport onto `release` as a separate PR.

## Related

- [Routes and announces](/quest/m1/cluster-routing/routes.md) - removes the hop list and with it the need for this cap
