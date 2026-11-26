# [L] A TS follow export carries on into a same-instance return while it drains

## Goal

When a followed broadcast's publisher leaves and the same instance returns within the export's delay, `moq export ts --follow` carries on into the return as soon as it sees it, while it is still playing out its buffer, so the output is continuous with no lost or duplicated frames. `export::the_same_instance_within_the_follow_continues` then passes every run. A different instance's return keeps its timeline restart.

## Plan

Found by the Nightly triage of run [45459585582](https://github.com/moq-dev/moq/actions/runs/45459585582) (`Tests (rs cli-export)`, panic at `rs/moq-cli/tests/integration/export.rs:605`). It fails about 1 run in 3 on `main` (e9a1671b4d). The 11-22 Nightly failed the same test differently ("export never exited", `export.rs:72`); look at that hang separately once this lands.

Cause, from instrumented runs: the relay sees the publisher gone about 0.9 s after the abort, and the same-instance return arrives at the same moment. `Follower::poll_state`'s `Running` arm (`rs/moq-mux/src/container/ts/follower.rs`) ignores `Serving::Return` until the export has drained its delay plus video lead (1.0 to 1.8 s). `Export::resume` then resubscribes every track at the live edge (`at_edge`) and drops frames whose release time passed. The test's tail after the export's first write varies from 7 to 12 chunks of 150 ms; with 7 the whole tail is late by the resume, and the assert fires. No single culprit PR: a one-run-per-commit bisect pointed at [moq-dev/moq#9515](https://github.com/moq-dev/moq/pull/9515), but its diff doesn't change behavior without dials, so it likely only shifted timing.

Decided 2026-11-25 (maintainer, /quest-plan): ✅ fix the follower (rejected: give the test margin with a smaller `--delay`, which leaves the lossy rejoin in place).

Recommended approach:

- In `Running`, react to `Serving::Return` once the program has left (`Feed::Retired`).
- Make `Export::resume`/`resubscribe` continue each track from its last delivered (group, frame) instead of the live edge; the same identity always carries the same content, so this is safe. `ExportSource::resuming` continues only at group granularity, which would skip the rest of the current group (this test's whole tail), and a replayed group's frames would trip the unmarked step-back timeline restart (`export.rs:546`), so continuation must be frame-level.
- Update the doc comment on `Export::leave`, which describes the drain-first behavior.
- Add a deterministic `export_test.rs` case on a mocked clock: the same instance leaves and returns before the drain ends, and the output is continuous without duplicates. Then loop `just rs cli-export` 20+ times.
