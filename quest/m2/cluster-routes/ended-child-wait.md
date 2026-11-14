# [S] An ended stream still waits on the child it taught

## Goal

`path::Router` holds a lost route until every neighbour that learned it from
this node has answered, as `RouteHandoff.tla`'s `Pending` does, including a
neighbour whose offer back arrives after this node ended the route on its
stream.

## Plan

Found 2026-11-14 by the mutation run ([moq-dev/moq#8712](https://github.com/moq-dev/moq/pull/8712)),
which excludes the one mutant it exposes (`&&` to `||` in
`path::Router::advertise`'s cleanup of ended streams) until this lands.

When a stream ends, `advertise` removes its sent entry unless the peer's route
is a pending child right then. The model keeps every ended entry's source
(`NoSent(t.src)`), so its `Pending` also counts a child offer that the
neighbour sent before it heard the END. With these steps, Rust sends peer 1
`Route { reach: None }` at the last step, while the model keeps waiting on
peer 2:

```
router 99; peers 1, 2, 3 on Link::default
route(1, 7, "", through(1)); batch(1, 7, "", [a/x = 1, b/x = 1], Some(1))
peer_interest 1: "", 2: "b", 3: ""
route(3, 7, "", along[(7,0), (6,1), (4,1), (1,1), (99,3), (3,1)])  // a child of the "" stream
route(2, 7, "b", along[(7,0), (99,9), (5,1), (2,1)])               // not a child
batch(1, 7, "", [b/x = None], Some(2))                             // the "b" stream to 2 ends
dead(1, closed([], false))                                         // "" holds under 1
route(2, 7, "b", along[(7,0), (6,1), (4,1), (1,1), (99,2), (2,1)]) // 2's offer back, sent before the END reached it
route(3, 7, "", None)                                              // peer 3 answers
```

(`through`, `along`, `closed` as in `route/tests/scope.rs`.) No loop or hold
cycle follows, since peer 2's stale path names this node, but the code departs
from the module doc's rule and from the checked model.

Recommendation, for the maintainer's review (taken by the agent under
/quest-afk): **align Rust with the model.** Keep an ended entry's source until
its stream is replaced or its session closes, make the steps above the test
that the route still holds, and drop the exclusion from `.cargo/mutants.toml`.
The model needs no change, so no new TLC run is required beyond the usual
`/verify-route`. Rejected: narrowing the model's `Pending` to the children
current when the stream ended, which weakens the checked rule to fit the code
and needs a new TLC run.

Public API: none. Wire: none.
