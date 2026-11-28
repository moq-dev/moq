# [M] Spike: what kio's waiter registration costs the relay fetch path

## Goal

A measured answer to how much of the relay fetch benches' time goes to `kio` wakeup machinery, led by `kio::WaiterList::register`, and which changes would win it back, with a prototype where one is cheap. The output is findings in this Plan and follow-up quests for the changes worth landing, not a landed optimization.

## Plan

Found by the Nightly triage of run [45459585582](https://github.com/moq-dev/moq/actions/runs/45459585582). `relay_session_delivery_fetch_frames` and `fetch_range` regressed with [moq-dev/moq#9422](https://github.com/moq-dev/moq/pull/9422) (+5 to 7% locally), which spread small accounting costs with no single hotspot. `kio::WaiterList::register` takes 12.9% of self time in `fetch_frames` both before and after #9422, so it is the bigger lever on the same benches.

Decided 2026-11-25 (maintainer, /quest-plan): ✅ a spike mostly into kio, in m3 (rejected: a fix quest on #9422's accounting alone; m1). Keep #9422's behavior. Profile on a pinned core with CPU-time measurement, then look at why `register` runs per fetch (re-registration on every poll, allocation, list contention) and what a cheaper shape would save.

## Related

- [Re-run release performance baseline after m2](/quest/m3/release/perf-recheck.md) - must fix or accept #9422's cost before 0.22
