# [M] The exhaustive route search replaces RouteHandoff.tla

## Goal

One exhaustive check guards the lite-07 route rule, and it runs the compiled
code: the search over every delivery order on `path::Router` also checks
`Seen`, `StaleIsChased`, and `NoStaleReturn`, and `RouteHandoff.tla`, its
configs, and the route trace validation this quest first planned are gone,
with every skill, quest, and doc that pointed at the model pointing at the
search instead.

## Plan

Decided 2026-11-10 (maintainer, /quest-plan): trace validation as the
strongest guard against the route model drifting from the code
([moq-dev/moq#8447](https://github.com/moq-dev/moq/pull/8447)), with resume,
reconnect, and subscribe each getting their own in m4. Moved to m3 on
2026-11-13 (maintainer, /quest-plan), since it waits on m2 work.

Found 2026-11-14 by the [feature review](/quest/m3/release/feature-review.md):
the exhaustive route search
([moq-dev/moq#8729](https://github.com/moq-dev/moq/pull/8729), merged
2026-11-14) searches every delivery order on the compiled `path::Router`,
faster than TLC on the same bounds (0.44 s against 2 s on the triangle) and on
meshes TLC cannot reach. `RouteHandoff.tla` now mostly duplicates it: only
`Seen`, `StaleIsChased`, and `NoStaleReturn` stay outside it, and a search
over the compiled code cannot drift from it, which was this quest's whole
purpose for route.

Decided 2026-11-14 (maintainer, pick on [moq-dev/moq#8764](https://github.com/moq-dev/moq/pull/8764)):
✅ keep the search and delete `RouteHandoff.tla` once #8729 lands (rejected:
keep the model and drop the search; keep both); #8729 has landed, so this is
ready. This replaces route trace
validation.

- Fold the three rules into the search as properties it checks, and show
  each one fails on a mutation that breaks it before deleting the model.
- Delete `rs/moq-net/verify/route/` and retarget or delete the
  `verify-route` skill and its Coverage table, REVIEW.md's Models pointers,
  and the quests that tell a PR to run `/verify-route`.
- The trace format and TLC recipe this quest was to build for the other
  models now come from the first of their m4 quests:
  [resume](/quest/m4/resume-trace-validation.md) builds them, and
  [reconnect](/quest/m4/reconnect-trace-validation.md) and
  [subscribe](/quest/m4/subscribe-trace-validation.md) reuse them.

Public API: none. Wire: none.
