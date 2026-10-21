# [XS] CI housekeeping

## Goal

Three small CI changes land together:

- The merge queue's nextest runs with `NEXTEST_TEST_THREADS=24`.
- Nightly `benchmark-site` and smoke `relays-report` delete an earlier
  attempt's stale `github-pages` artifact before uploading, as interop's
  report job does.
- `.github/actionlint.yaml` drops its ignore for the `queue` key once an
  actionlint release after 1.7.12 is in the dev shell.

## Plan

- **Threads.** Recommended in
  [moq-dev/moq#6174](https://github.com/moq-dev/moq/pull/6174): the suite
  needs about 200 CPU-seconds, and most slots wait on sockets, subprocesses,
  and timeouts. Pinned to 8 CPUs it took 105 to 113 s at 8 threads and 47 s
  at 24. Set it for the queue job only, after a few loaded
  `just check --all` runs at 24; a flake there points at a test to fix.
- **Pages artifact** and **actionlint** both come from
  [moq-dev/moq#6180](https://github.com/moq-dev/moq/pull/6180). A re-run of
  either nightly job fails on the stale artifact today. The actionlint fix,
  [rhysd/actionlint#661](https://github.com/rhysd/actionlint/pull/661), is
  merged but unreleased; if no release reaches nixpkgs by the time the rest
  is ready, land the rest and leave this bullet.

Public API: none. Wire: none.

## Related

- [CI workflow audit](/quest/m1/audit-ci.md) - the broader per-job review
