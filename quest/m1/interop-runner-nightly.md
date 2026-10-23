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


Implementation checkpoint: [moq-dev/moq#6257](https://github.com/moq-dev/moq/pull/6257)
prepares the dynamic published-image matrices and the separate artifact publisher.
It is unfinished and must stay a draft until these conditions are met:

- Upstream must distinguish setup failures from client verdicts. Today
  `run-interop-tests.sh` returns 1 and records `status: fail` for both a
  failed Docker Compose startup and a failed third-party test. A complete
  summary cannot distinguish these. The checkpoint fails closed on all
  nonzero runner exits; do not make it report-only by accepting exit 1.
  Recommendation: an upstream summary field or distinct exit code for setup
  failures, then accept only explicitly classified test verdicts locally.
- Run both matrices with real Docker Compose and the source-built relay,
  including an injected Compose startup failure and a third-party test
  failure. This host has rootless Podman but no Compose provider.
- Reuse the stale Pages artifact cleanup from
  [moq-dev/moq#6207](https://github.com/moq-dev/moq/pull/6207) once available;
  do not duplicate its helper here. Verify publishing and reruns.
- Confirm the new directory appears in the shared Pages root after the
  benchmark writer regenerates its directory-based index.


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
