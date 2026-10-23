# [M] Nightly runs the upstream interop runner

## Goal

A nightly job runs [englishm/moq-interop-runner](https://github.com/englishm/moq-interop-runner)
in its docker mode at its default branch's HEAD, with a moq-relay image
built from `main` standing in for the `moq-dev-rs` relay. Every client and docker-only
relay registered upstream with a published image is exercised against our
relay and our clients, so such an implementation added to the registry is
tested here without a change on our side. Entries with only a local-build
recipe are reported as not run. The runner's own HTML report publishes to
<https://git.moq.dev/interop-runner/>. Third-party verdicts never fail the
nightly, but a failure in the runner's own setup (checkout, startup, or
report generation) does.

## Plan

Decided with the maintainer on 2026-10-21.

- Reuse the runner's tooling rather than teaching `test/smoke/relays.sh` to
  start docker relays and clients. That keeps one implementation of the
  registry's client contract.
- Track the runner's HEAD, not a pinned commit. The registry lives in the
  runner's checkout, so a pin would freeze it and hide every implementation
  registered after it until the next [interop audit](/quest/m1/audit-interop.md).
  The Cloudflare and Moxygen smoke lanes already build and run unpinned
  third-party HEAD the same way.
- Split the work the way `interop.yml` does, so upstream code never runs
  with credentials. The runner job has read-only permissions and no
  secrets, and uploads the generated report as an artifact. A separate
  publish job only copies that artifact under its own Pages directory with
  the per-directory publisher (`sh/gh/pages-publish.sh`); it is another
  `gh-pages` writer, so it joins the `benchmark-pages` concurrency group
  with `queue: max`. The remaining exposure is upstream-written HTML on a
  static site that holds no credentials. The Pages root lists it.
- Report only for third-party verdicts. Pairs fail for reasons outside this
  repo, and the interop audit owns the follow-up, so no cell gates the run.
  A failure in the runner's own setup (checkout, image startup, or report
  generation) fails the job, so a total loss of coverage or a stale report
  shows up.
- Recommendation: run it from `smoke.yml` next to the public relays lane,
  since both exercise third-party implementations and share its schedule.
- Images the registry only describes as a local build (no published image)
  are skipped and listed as not run, not built here.
- Add a lane to `test/smoke/README.md`'s table.

Public API: none. Wire: none.

## Related

- [Interop audit](/quest/m1/audit-interop.md) - audits this job's moq.dev cells each run
