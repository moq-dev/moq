# [M] Instruction counts from stock Valgrind instead of CodSpeed

## Goal

`.github/workflows/codspeed.yml` counts each benchmark's instructions with kixel-ai/rust2's Valgrind runner, on stock Valgrind, instead of CodSpeed's Action and its Valgrind fork, and uploads the runner's `metrics.json` as its `kixel-metrics` artifact.
It runs nightly on `main` on hosted `ubuntu-24.04`, plus a `workflow_dispatch` whose `benchmarks` input names the benchmarks a Kixel Quest bisect step measures, so each step measures only the suspects.
The job loses `id-token: write` and the CodSpeed upload, and splits into shards only if one job would take too long.
Any developer or agent runs the same count locally with one `just` recipe and compares two commits, getting the number CI reports.

Non-goals: rewriting benchmarks (sources keep Criterion's API through `codspeed-criterion-compat` until rust2's harness lands, and that epic files its own migration quest here), Nightly's paired wall-clock matrix, which stays as it is, and runs on PRs.

## Plan

Decided 2026-11-23 (maintainer, `/quest-plan` in kixel-ai/kixel.quest, [kixel-ai/kixel.quest#113](https://github.com/kixel-ai/kixel.quest/pull/113)), asked "is there anything like codspeed that we could run locally instead of using them as a 3rd party provider?":

- ✅ Replace CodSpeed with stock Valgrind (nixpkgs) through our own runner, reporting only to Kixel Quest, on any Linux host (rejected: moq only for now; keeping CodSpeed). Checked 2026-11-23: stock Valgrind 3.27.1 honors the per-benchmark markers CodSpeed's compatibility crates emit, writing one Callgrind dump per benchmark named after it. The two requests only CodSpeed's fork understands print a warning.
- ✅ Our own parallel runner in kixel-ai/rust2 over those markers, and a Criterion replacement planned there (rejected: rewriting to Gungraun, which counts only and would leave every benchmark written twice beside Nightly's wall-clock; only our own harness).
- ✅ Nightly on `main`, with Kixel Quest bisecting each regression to its PR, superseding the 2026-11-22 decision in `quest/m0/kixel-metrics.md` to run on each landing (rejected: coalesced runs on each landing). The maintainer: "I don't want to waste CPU cycles running every benchmark on every commit. Bisecting is always going to be cheaper because it's logN and can target a specific benchmark."
- ✅ No benchmark runs on PRs, since none gates. Agents compare locally (rejected: per-PR tables in the job summary).
- ✅ GitHub-hosted runners, free for this public repository (rejected: self-hosted, whose cores agents and the merge queue share).

Recommendations for the implementer:

- [moq-dev/moq#9472](https://github.com/moq-dev/moq/pull/9472) was open when this was planned, putting CodSpeed on each landing and parsing Callgrind profiles in `sh/gh/bench-report.py`. If it merged per-landing, restore the nightly schedule here, keeping `workflow_dispatch` and the per-ref `concurrency` group bisects need. Delete its Callgrind parsing in favor of the runner's `metrics.json`; keep the Nightly converter.
- Pin the runner and its Valgrind through the rust2 flake input moq already has, so a Valgrind upgrade, which can shift counts, lands as a visible pin bump.
- Series names from the runner must match what `codspeed-metrics` wrote, or say in the PR that the series restart.

Public API: none. Wire: none.

## Waiting

- kixel-ai/rust2: its Valgrind runner lands in [kixel-ai/rust2#995](https://github.com/kixel-ai/rust2/pull/995); check `gh pr view 995 -R kixel-ai/rust2 --json state,mergeCommit`. moq's `rust2` flake input pins that PR's head (`af1a7664`) until then; move the pin to its squash commit on rust2's `main`.
