# [S] Nightly's shipping-size check passes

## Goal

Nightly's shipping-size check passes on `main`. Every growth over its 5%
threshold is traced to the commits that caused it. Each is then shrunk back,
or accepted as a new baseline with the reason recorded.

## Plan

Decided 2026-11-03 (maintainer, /quest-plan of the follow-ups from
[moq-dev/moq#7667](https://github.com/moq-dev/moq/pull/7667)): milestone m0,
a child of the Nightly epic.

The [2026-11-03 shipping-size job](https://github.com/moq-dev/moq/actions/runs/41656613559/job/126039954216)
failed nine daily comparisons between
[dde50c699f](https://github.com/moq-dev/moq/commit/dde50c699f69507ced243754c2f700a6184d1576)
and [801fd9a2f2](https://github.com/moq-dev/moq/commit/801fd9a2f2f5c88371fa833d805620d97d372465).
[Investigation PR](https://github.com/moq-dev/moq/pull/7813) records every failing
measurement and its attribution evidence:

- Rebuilt both JS endpoints and all 18 intervening JS/lockfile commits. All 21
  retained endpoint measurements reproduce exactly. Net raw grows 22,656 bytes;
  watch raw grows 30,479. Subscription ranges contribute 17,090 and 17,111
  respectively, and stats/feedback adds 6,787 watch bytes. The remaining deltas,
  including every failing compression format, are attributed in the PR.
- Bigint group IDs are excluded as a cause: their merge happened after the
  failed Nightly commit.
- All four downloaded relay archives and executable code sections grew. Native
  x86_64 Linux symbol attribution identifies new rind and rind-rustls alongside
  transport's retained AWS-LC. The PR separates local diagnostic builds from
  the actual shipping archive sizes. Native per-commit attribution on macOS,
  aarch64 Linux, and Windows MSVC remains unfinished; do not label the Linux
  measurements as those targets' results.
- The daily baseline already advances after a threshold failure. The failed
  run retained its current point, and there was no thirty-day anchor yet.
  `sh/size/README.md` documents acceptance using retained history, and tests
  cover both daily advancement and the surviving thirty-day guard. Summaries
  now identify the current and failing baseline commits.

Remaining work is the other targets' relay attribution, the acceptance
decisions below or reductions that retain the required behavior, and scheduled
evidence on main. Keep the 5% and 10% limits and the retained history intact.
[Size audit](/quest/m2/audit-size.md) owns the release-time view.

Public API: none. Wire: none.

## Waiting

- Maintainer: accept the exactly attributed JS feature and correctness costs,
  or require further reduction while retaining those behaviors. Recommend
  accepting the measured JS step; the PR records the consumers and byte costs.
  Relay acceptance stays open until the other targets' attribution is supplied,
  or the maintainer explicitly accepts the evidence limit in the PR.
- moq-dev/moq: historical Windows MSVC measurements need a matching build host;
  none of the listed SSH hosts runs Windows. Supply before/after archive
  measurements for the responsible commits, rather than substituting Linux
  `cargo bloat` estimates. macOS and aarch64 Linux attribution also remains work.
- moq-dev/moq: after the change lands, inspect the next scheduled Nightly
  shipping-size job on main, its measured revision and retained baseline, for
  passing comparisons. No manual Nightly dispatch; retain this quest until that
  evidence and the remaining attribution/acceptance work are settled.

## Related

- [Size audit](/quest/m2/audit-size.md) - pre-cut size investigation and the post-release refresh
