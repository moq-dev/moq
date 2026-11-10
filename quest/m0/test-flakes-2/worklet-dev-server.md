# [S] js/common's worklet dev-server test holds up under load

## Goal

`js/common/worklet.test.ts`'s "the dev server serves hosted worklets where
builds emit them" passes on a loaded host, and its `test.skip` is removed. The
fix lands at the cause of the timeout, never by raising bun's 5 s test timeout
or adding a retry.

## Plan

Decided 2026-11-09 (maintainer, `/quest-complete`), under the repository's
flaky-test rule: quarantine the test and plan a quest. Rejected: leave it
enabled and watch it.

Evidence: a full `just check` on
[moq-dev/moq#8325](https://github.com/moq-dev/moq/pull/8325)'s branch, with the
host loaded, hit bun's 5 s timeout in `js/common/worklet.test.ts`. The test
passed alone and on a second full run. The log of the failed run was not kept,
so which test timed out is unconfirmed.

Candidate, unconfirmed: the dev-server test. It is the only one that opens real
sockets, starts a Vite dev server, makes several requests, and awaits
`server.close()` in its `finally`. Alone on 2026-11-09 it took 322 ms, against
208 ms for the lazy-blob test. The file reported 4.98 s for its two tests,
though their bodies took about 0.5 s together. The rest of that time is outside
the test bodies and was not measured, so it is not evidence either way.

Reproduction attempts, all on 2026-11-09, all passing:

- `bun test js/common/worklet.test.ts --rerun-each 60` on the already loaded
  host: 120 of 120.
- The same with 48 CPU burners and `--rerun-each 150`: 300 of 300.
- The root JS set (`api`, `constructors`, `deps`, `worklet`, `release`, which
  bun runs in one process) with 48 burners and `--rerun-each 20`: 640 of 640.

Reproduced 2026-11-10 on a 24-core host with 64 `yes` burners and the skip
removed, `--rerun-each 150`: both tests in the file time out at 5 s, the
enabled lazy-blob test as often as the dev-server one, so the dev-server test is
not uniquely fragile. Per-step timing with 48 burners put the time in real CPU
work, not a hung wait: one run spent 2.5 s in the final `fetch` (the middleware's
first `rolldown` compile of the worklet), another 1.4 s in `server.listen()`.
`server.close()` took under 50 ms every time. No step waits on an event the
test could observe instead; the budget is lost to starved compute, which the
lazy-blob test (two compiles) pays too.

The earlier round already fixed other `worklet.test.ts` flakes, so this is a new
cause, not a regression of those.

Public API: none. Wire: none.

## Waiting

- Maintainer: choose how to treat a test whose 5 s budget only fails when the host is CPU-starved, since no event-based fix exists and the goal rules out a longer timeout. Options: (1) remove the `test.skip` as-is, since the enabled sibling carries the same exposure and both passed 640 of 640 in the root set under 48 burners on 2026-11-09; (2) move both tests to the nightly `test/worklet` matrix; (3) keep the quarantine. Recommended: (1). Done when this quest is rewritten to the chosen option.
