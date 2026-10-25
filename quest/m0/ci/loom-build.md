# [S] Loom tests compile, and the gate proves it

## Goal

`just rs loom` builds and runs on `main`, and a PR that breaks the `--cfg loom`
build fails its own gate instead of the next Nightly.

## Plan

Current main already ports the old `Subscription::with_groups` call to
`with_range`. The first local CI run with the new compile gate exposed two
`broadcast::Producer::route` calls made private by
[moq-dev/moq#6744](https://github.com/moq-dev/moq/pull/6744); the model now
observes the route through the public origin consumer instead.

The implementation reuses `just rs loom --no-run` in the selected Rust
check/lint lane when kio or moq-net changes. Selection tests cover those
owners, unrelated crates, and avoiding duplicate work in the test lane.

Remaining validation:

- Complete compilation of all three Loom targets with the existing recipe.
- Run the full `just rs loom`, recording the known execution failures separately.
- Re-run `just ci` after the stale model is ported. The first run passed the
  script checks and failed only on the private-method compile errors above.

The Loom models that exceed their branch limit or run past the job timeout
are tracked in [CI audit](/quest/m2/audit-ci.md); this quest restores the build
and prevents another compile regression.

Public API: none. Wire: none.
