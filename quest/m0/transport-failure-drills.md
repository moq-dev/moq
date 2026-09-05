# [L] Reproducible transport and lifecycle failure drills

## Goal

Protocol and lifecycle fixes can be checked against real disrupted sessions and
repeatable concurrency regressions before merging, with proof that the asserted
failure path was actually exercised.

## Plan

`just rs loom` already explores modeled kio/moq-net handoffs, and `just rs fuzz`
already fuzzes wire codecs with stable corpus replay in ordinary tests. Build
on those mechanisms. Successful local media flow does not prove cancellation,
loss recovery, or teardown under backpressure.

- Add a reusable local relay/client drill recipe with three bounded scenarios:
  stall a reader and cancel under backpressure, terminate a relay mid-group,
  and interrupt then restore a publisher before republishing the same name.
  Assert terminal results, resumed delivery where promised, and resource release.
- Add an isolated Linux network-namespace profile for bidirectional UDP delay,
  loss, and rate limits. Record settings and seeds plus measured impairment;
  a seed does not make kernel scheduling deterministic. Do not substitute an
  HTTP interceptor or TCP-only proxy for QUIC impairment.
- Keep exact race ordering in in-process tests using barriers and existing Loom
  primitives. Assert that the contested path occurred; an arbitrary sleep or a
  test that passes because no frames arrived is not a regression.
- Demonstrate sensitivity by removing a selected fix in a disposable source
  snapshot and showing the focused test fails for the intended reason. A compile
  failure is not behavioral proof. Never mutate the developer's active checkout.
- Document which focused Loom cases and existing fuzz regressions exercise each
  drill's underlying primitive. Preserve failure inputs and seeds; leave CI lane
  scheduling to the PR behavioral gates quest and do not use retries to make
  intermittent failures green.

Acceptance: the three drills fail when their recovery/cleanup behavior is
disabled and pass with it restored. Include a no-publisher negative control,
record observed fault activation, and prove cancellation removes owned network
namespaces and processes. Privileged impairment setup stays isolated to the
test runner; it must not change the host's normal network path.

## Related

- [PR behavioral gates](/quest/m0/pr-behavioral-gates.md) - selects bounded scenarios by changed scope
- [Failure artifacts](/quest/m0/qa-failure-artifacts.md) - stores timelines, seeds, and traces
- [Runtime QA hosts](/quest/m2/runtime-qa-hosts.md) - provides Linux execution for privileged drills
