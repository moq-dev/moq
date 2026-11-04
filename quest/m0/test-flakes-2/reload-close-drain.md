# [S] reload.test.ts's close-drain tests pass under load

## Goal

The close-drain tests in `js/net/src/connection/reload.test.ts` pass under
load in multi-file runs, each fixed at its cause under the
[epic](/quest/m0/test-flakes-2/README.md)'s rules. Done when the PR names the
failing case, explains the race, and shows the repeated multi-file run that
reproduced it now passing.

## Plan

Planned 2026-11-04 with the maintainer. While iterating
[moq-dev/moq#7090](https://github.com/moq-dev/moq/pull/7090), its agent saw a
close-drain timing test in `reload.test.ts` fail once in a multi-file run, then
pass 13 runs with and without that PR's change; the failure output wasn't kept.
Candidates are the drain cases: `teardown by … drains the session`, `a URL
change connects the replacement while the old session drains until …`, and
`closing the loop keeps the tokens until the session drains`.

Reproduce first: run `reload.test.ts` together with the rest of `js/net` under
CPU contention and `--rerun-each`, as the epic's earlier JS fixes did
([moq-dev/moq#7658](https://github.com/moq-dev/moq/pull/7658) fixed the
close-drain fixture's setup-group race the same way). If it doesn't reproduce
in a reasonable number of runs, report that and the runs tried rather than
changing the test blind.

Public API: none. Wire: none.
