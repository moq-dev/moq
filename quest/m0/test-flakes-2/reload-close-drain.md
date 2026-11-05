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

Reproduce first: run `reload.test.ts` together with the rest of `js/net` under
CPU contention and `--rerun-each`, as the epic's earlier JS fixes did
([moq-dev/moq#7658](https://github.com/moq-dev/moq/pull/7658) fixed
`lite/connection.test.ts`'s close-drain fixture race that way). If it doesn't
reproduce in a reasonable number of runs, report that and the runs tried
rather than changing the test blind.

Public API: none. Wire: none.
