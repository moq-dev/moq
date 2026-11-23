# [M] Benchmarks reach Kixel Quest on every landing

## Goal

CodSpeed runs on every push to `main`, coalesced so only the newest pending commit waits behind a running one, and it and Nightly each upload a `kixel-metrics` artifact that Kixel Quest reads.
A regression then becomes an investigation quest in this repository's m0, bisected to its culprit PR, instead of a row on https://git.moq.dev/bench/ that nothing reads.
No PR waits on any of it.

Non-goals: making Nightly or CodSpeed pass, which [Nightly](/quest/m0/nightly/README.md) and [moq-dev/moq#9089](https://github.com/moq-dev/moq/pull/9089) own; per-PR benchmark runs; moving the paired wall-clock matrix off its nightly cadence.

## Plan

Decided 2026-11-22 (maintainer, `/quest-plan` of the follow-ups of [kixel-ai/kixel.quest#91](https://github.com/kixel-ai/kixel.quest/pull/91), which plans Kixel Quest's [measurement loop](https://github.com/kixel-ai/kixel.quest/blob/main/quest/m0/optimize/README.md)):

- ✅ CodSpeed on each landing, coalesced, with Nightly's paired matrix staying nightly (rejected: Nightly's full matrix coalesced on each landing, about a dozen two-hour runs a day on the self-hosted runners agents share, with timings that host load makes noisy; Nightly only, which leaves Kixel Quest bisecting across a day's ~90 landings). CodSpeed counts simulated instructions, so host load doesn't move it. It already runs on GitHub-hosted `ubuntu-24.04` in about 26 minutes, at most about 55 runs a day when coalesced.
- ✅ Kixel Quest pulls the artifact when a run finishes, so the workflow holds no write token (rejected: posting results with an OIDC token).
- Benchmarks never gate the merge queue (decided 2026-10-20), and this keeps it so.
- m0: today's results sit unread, which is the problem this fixes. It waits on Kixel Quest, so it takes no capacity until then.

Recommendations for the implementer:

- Coalesce with a `concurrency` group that doesn't cancel in progress: GitHub then keeps only the newest pending run. Kixel Quest dispatches the bisect runs itself through `workflow_dispatch`, so the workflow takes a commit input.
- Write the artifact from what `just gh benchmark-report` already collects, rather than a second converter per source.
- Until CodSpeed's `mux` shard passes (it panics at `rs/moq-mux/benches/export.rs:172` on every run since 2026-11-14), its series are missing, not zero.

The Kixel Quest App is installed on moq-dev (checked 2026-11-22): it was installed on selected repositories, moq-dev/moq among them, not all repositories, with read access to commit statuses, issues, and metadata, and read and write access to actions, administration, checks, code, organization self-hosted runners, pull requests, and workflows.
https://kixel.quest/repos lists moq-dev/moq, though its index then failed on the installation's GitHub API rate limit.

Public API: none. Wire: none.

## Waiting

- kixel-ai/kixel.quest: its [metrics](https://github.com/kixel-ai/kixel.quest/blob/main/quest/m0/optimize/metrics.md) quest ships the `kixel-metrics` format and ingest; check that `quest/m0/optimize/metrics.md` is gone from kixel.quest's `main`.
