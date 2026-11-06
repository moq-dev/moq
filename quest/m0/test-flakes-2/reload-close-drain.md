# [S] reload.test.ts's close-drain tests pass under load

## Goal

`closing the loop keeps the tokens until the session drains` in
`js/net/src/connection/reload.test.ts`, and any drain case sharing its race,
passes under load in multi-file runs, fixed at its cause under the
[epic](/quest/m0/test-flakes-2/README.md)'s rules. Done when the PR explains
the race and shows the repeated multi-file run that reproduced it now passing.

## Plan

Planned 2026-11-04 with the maintainer. While iterating
[moq-dev/moq#7090](https://github.com/moq-dev/moq/pull/7090), its agent saw
`closing the loop keeps the tokens until the session drains` fail once in a
four-file batch, then pass 13 runs with and without that PR's change. Its
failure output wasn't kept. The other drain cases in the file also run on a
manual clock and wait for the old session to close: `teardown by … drains the
session` shares its held-FIN fixture (`holdFins`), and `a URL change connects
the replacement while the old session drains until …` waits out a drain
deadline. Check them for the same race.

The gated `just ci origin/main` on the JS socket branch reproduced this on
2026-11-05: `fin.closed` was already true after advancing 999 ms, where the
assertion expected false. The standalone full `js/net` run passed this case.
[moq-dev/moq#7884](https://github.com/moq-dev/moq/pull/7884) quarantines it
with every assertion retained; restore it when this quest fixes the race.

Reproduce first: run `reload.test.ts` together with the rest of `js/net` under
CPU contention and `--rerun-each`, as the epic's earlier JS fixes did
([moq-dev/moq#7658](https://github.com/moq-dev/moq/pull/7658) fixed
`lite/connection.test.ts`'s close-drain fixture race that way). If it doesn't
reproduce in a reasonable number of runs, report that and the runs tried
rather than changing the test blind.

Decided 2026-11-05 (maintainer, /quest-plan): keep this quest and investigate
the next captured failure with logs. Abandoning it was rejected: the agent on
[moq-dev/moq#7764](https://github.com/moq-dev/moq/pull/7764) saw
`closing the loop keeps the tokens until the session drains` fail once in a
full-suite run on 2026-11-05. It reported `expect(fin.closed).toBe(false)`
failing after a 999 ms clock advance, then three passing isolated runs.
This sighting shows the flake still occurs despite the passing reruns below;
it does not establish the cause. Keep this wait in the quest so the
evidence-only [moq-dev/moq#7933](https://github.com/moq-dev/moq/pull/7933)
can land without a speculative test or production change.

[moq-dev/moq#7764](https://github.com/moq-dev/moq/pull/7764) also disables
the test (`test.skip`, "load-sensitive fin.closed assertion; investigate drain
scheduling before restoring") under the repo's disable-a-flaky-test policy, and
says no follow-up quest was created; this quest is that follow-up. Once that
skip lands, restoring the test is part of this quest: a skipped test cannot
meet the Goal or Done condition, and failures can only be captured by
re-enabling it (for example, with per-dial diagnostics) in a multi-file run.

On the next captured failure, check the dial count and which client closed
first. In `closing the loop keeps the tokens until the session drains`, the
fixture overwrites shared `fin.hold` on every dial and lets any
client's close set shared `fin.closed`, but reads the track from `dials[0]`.
An extra dial could therefore hold FINs on another client or report another
client's close. The `teardown by …` fixture does not share this hazard: it
sets `fin.hold` and `fin.closed` only on the first dial
(`if (dials.length === 0)`). This is an unverified hypothesis, not an established cause;
check it alongside setup-group completion before choosing a fix.

Public API: none. Wire: none.

### Reproduction attempts

2026-11-04, recorded in [moq-dev/moq#7933](https://github.com/moq-dev/moq/pull/7933):
no reproduction on the unchanged claim based on `origin/main`.

- `quest gate bun test --cwd js/net --rerun-each 20 --only-failures`
  finished with 58,645 passes, 40 skips, and 95 failures across 121 files
  (476.88 seconds). All 20 reload-file rounds passed. The package
  batch also exposes unrelated rerun failures in the benchmark import test
  and connection fallback tests; it is not a clean package validation.
- A four-file batch (`connection/reload`, `connection/pool`, `connection/close`,
  and `lite/connection`) filtered to `closing the loop|teardown by|a URL change`
  with `--rerun-each 5` passed 40 tests, zero failures. A scratch preload
  stubbed `performance.now()` to advance 5 ms on each budget clock read,
  forcing the shared stream budgets to yield at each clock check. This
  stresses budget scheduling only: it neither advances the test's manual
  clock nor forces an extra dial or another client's early close, so it
  does not rule out those paths.
- The earlier lite fixture fix suggests checking setup-group completion,
  but neither run supports changing this fixture yet. No test or production
  code changed, per the reproduce-first requirement.

## Waiting

- moq-dev/moq: capture the next reload drain failure with logs before choosing
  a fix. If #7764's skip has landed, no full-suite failure can occur; restore
  the test with per-dial diagnostics and reproduce under multi-file load instead. Check full-suite and merge-queue failure reports for the test names
  above; retain the full command, commit, and assertion/stack output in the
  report, plus any available session logs. The fixture currently emits no
  dial/close diagnostics; those logs are not required to resume investigation.
  Once captured, link that report here, remove this waiting item, and reproduce
  the failure under multi-file load, capturing dial count and per-client close
  state to test the hypothesis above. Passing isolated reruns alone do not
  clear this condition.
