# [M] Nightly runs the upstream interop runner

## Goal

A nightly job runs [englishm/moq-interop-runner](https://github.com/englishm/moq-interop-runner)
in its docker mode at a pinned commit, with a moq-relay image built from
`main` standing in for the `moq-dev-rs` relay. Every client and docker-only
relay registered upstream is exercised against our relay and our clients,
so an implementation added to the registry is tested here without a change
on our side. The runner's own HTML report publishes to
<https://git.moq.dev/interop-runner/>. The job never fails the nightly.

## Plan

Decided with the maintainer on 2026-10-21.

- Reuse the runner's tooling rather than teaching `test/smoke/relays.sh` to
  start docker relays and clients. That keeps one implementation of the
  registry's client contract, and the [interop audit](/quest/m0/audit-interop.md)
  only bumps the pin.
- Publish the runner's generated report as is, under its own Pages
  directory, with the per-directory publisher
  (`test/smoke/report/publish.sh`). It is another `gh-pages` writer: join the
  `benchmark-pages` concurrency group with `queue: max`, as the
  [interop report](/quest/m1/interop-report.md) does. The Pages root lists it.
- Report only. Third-party pairs fail for reasons outside this repo, and the
  interop audit owns the follow-up, so no cell gates the run.
- Recommendation: run it from `smoke.yml` next to the public relays lane,
  since both exercise third-party implementations and share its schedule.
- Images the registry only describes as a local build (no published image)
  are skipped and listed as not run, not built here.
- Add a lane to `test/smoke/README.md`'s table.

Public API: none. Wire: none.

## Related

- [Interop audit](/quest/m0/audit-interop.md) - bumps this job's runner pin and audits its moq.dev cells each run
- [Interop report](/quest/m1/interop-report.md) - the in-tree interop matrix's own Pages report
