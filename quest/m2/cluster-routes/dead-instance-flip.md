# [M] A dead instance's stale path never wins a lite-07 route

## Goal

A reader of a path whose route flips C → B → C never sees B when B is an instance already known dead, whenever it polls, so the route-sweep invariants hold with no window for a step back to a dead instance.

## Plan

Found by [moq-dev/moq#9108](https://github.com/moq-dev/moq/pull/9108), which fixed the Nightly `mesh_sweep` failures: when the route flipped C → B → C before the reader polled, the reader got a second restart to C. #9108 merges a restart back to the instance the reader already shows into an Update, but a reader that polls mid-flip can still see B, so the sweep can still flake.

The flip became reachable with the lite-07 route rule in [moq-dev/moq#8987](https://github.com/moq-dev/moq/pull/8987). Decide whether that rule should stop a dead instance's stale path from winning at all, which removes the flip, rather than hiding it at the reader. Recommended: stop it at the rule if the route model (`/verify-failover`, `/verify-warm`) shows no case where the stale path must win; check the draft's route section for the same.

Public API: none. Wire: possibly the lite-07-wip route rule; update the draft in the same PR if so.
