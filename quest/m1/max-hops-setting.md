# [S] A relay setting bounds path length

## Goal

A relay operator can set the longest hop path a relay accepts or forwards,
with today's 32 as the default. On a mesh whose diameter is a few hops, a
cap near diameter plus a margin stops lite-06 path hunting from exploring
long detours after a withdrawal. It ships with the next cut from `main`; the
maintainer decided on 2026-10-22 not to backport it to `release`.

## Plan

The draft implements the settled relay setting as `cluster.max_hops`,
`--cluster-max-hops`, and `MOQ_CLUSTER_MAX_HOPS`: default 32, refusing 0 and
values above 32. Shorter paths keep their existing ranking. The origin owns
the policy so all accepted and dialed sessions share it, including scoped
handles. The proposed Rust API is `origin::Config.max_hops: origin::HopLimit`,
with `HopLimit::new(usize) -> Option<Self>` and `get()`.

The implementation counts the complete advertised path, including the
sender's hop even when Lite makes the receiver append it. It covers
moq-lite-03 through moq-lite-07-wip and the same hop path in the negotiated
MoQ Cluster extension on moq-transport drafts 17 through 22. Lite01/02 and
moq-transport without the extension hide their depth and ignore the setting.
Once [Routes and announces](/quest/m1/cluster-routing/routes.md) removes
Lite07's hop list, the setting must stop applying to that version.

Focused coverage checks receive and send admission, replacement/update of a
previously admitted path, policy inheritance, and CLI/env/TOML precedence.
Across 200 seeded meshes of 8 to 17 relays, a cap of diameter plus two lowers
extra withdrawal observations from 192 to 185, hunted relays from 179 to 177,
and revivals from 3 to 2. Every mesh ends retracted; the reduced residual
counterexamples are also checked at the lower cap. This bounds detours rather
than eliminating the remaining path-hunting cases.

Validation on the proposed implementation: full Nix `just ci`,
`just test interop --all`, the focused protocol/configuration/mesh tests,
and the relay's `--help` all pass.

Remaining maintainer decisions:

- Accept the typed Rust origin policy API, or choose another policy boundary.
  Recommend the typed origin field: invalid caps cannot reach a session, and
  one origin keeps every link consistent without changing client/server APIs.
- The original scope said "hop-list versions" but only listed Lite. Confirm
  the same cap should cover IETF's negotiated cluster hop path too. Recommend
  the implemented shared scope because it has identical path semantics and
  can expose the same detour cost. Clearer wording: "Apply the cap to every
  protocol that carries the origin's hop path: Lite03 through Lite07-wip and
  moq-transport 17+ with MoQ Cluster; paths with no visible hop list are outside
  the cap's guarantee."

Operators size the cap to the topology's diameter after any single link or
node failure plus margin, counting the publisher hop. A cap below that can
partition the mesh. No wire encoding changes or breaking API changes are
proposed, so no migration is required; existing configurations keep 32. The
policy is local to the native relay, with no new JavaScript relay consumer or
binding surface. The maintainer already decided not to backport to `release`.

## Related

- [Routes and announces](/quest/m1/cluster-routing/routes.md) - removes the hop list and with it the need for this cap
