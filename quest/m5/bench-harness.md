# [XL] Benches move from Criterion to swole

## Goal

Every Rust bench target in moq is written as [swole](https://github.com/kixel-ai/swole) `#[bench]` functions, which swole counts and Nightly times through its paired timing. Criterion, `codspeed-criterion-compat`, `rs/bench-harness`, and `cfg(codspeed)` leave the workspace, and `.github/workflows/codspeed.yml` is renamed after what it runs, such as `instructions.yml`, so nothing in moq names CodSpeed.

Non-goals: what each bench measures, and how often Nightly and the instruction counts run.

## Plan

Decided 2026-11-25 (maintainer, `/quest-plan` in kixel-ai/rust2, [kixel-ai/rust2#1031](https://github.com/kixel-ai/rust2/pull/1031)):

- ✅ No CodSpeed service, app, or code in moq-dev or kixel-ai (rejected: the service and app only; drop the work). moq stopped using the service in [moq-dev/moq#9555](https://github.com/moq-dev/moq/pull/9555). What's left is the crate and the name.
- ✅ Rewrite the benches to `#[bench]` (rejected: a Criterion-API count layer in the harness, deleted later by this rewrite; wait for rust2's m2 harness epic).
- ✅ The harness's wall-clock timing ships first, so each bench is written once and Criterion leaves with CodSpeed's crate (rejected: rewrite now and stop Nightly's wall-clock until timing ships; keep a Criterion copy of each bench for timing). Paired timing, which shipped in [kixel-ai/rust2#1033](https://github.com/kixel-ai/rust2/pull/1033), is required to cover what Nightly times today, including the async benches and the `iter_custom` ones that keep an input and time part of each iteration.

Decided 2026-11-25 (maintainer, `/quest-plan` in kixel-ai/rust2, [kixel-ai/rust2#1041](https://github.com/kixel-ai/rust2/pull/1041)): ✅ rust2-bench and its harness move to [kixel-ai/swole](https://github.com/kixel-ai/swole) as `swole` before this rewrite, so the benches are written against `swole` once instead of renamed afterward. The move is [kixel-ai/rust2#1046](https://github.com/kixel-ai/rust2/pull/1046); the harness is `swole = { git = "https://github.com/kixel-ai/swole" }` in `[dev-dependencies]` until its first crates.io release.

m9: it touches only benches, CI, and docs, so it changes no published API, wire format, or CLI surface, and it needn't wait for the release cut.

Recommendations for the implementer:

- One PR: Criterion can leave only once every target has moved, and a half-moved tree makes Nightly and `sh/rs/instructions.sh` run both harnesses.
- Keep each benchmark's name where the harness can (its file, then module, function, and argument), so Kixel Quest's instruction series continue instead of restarting. Name in the PR the ones that change.
- The rewrite is a boundary for Nightly's paired timing: a base before it has only Criterion benches. Recommended: for a base before the rewrite, Nightly times the head alone and says the pair was skipped, so the first night reports without keeping `bench/run.sh`'s `--criterion` path alive. Delete that path and its `BENCH_BASE` scripts once the harness replaces them.
- `sh/rs/instructions.sh` selects targets by what CodSpeed's Criterion couldn't count, such as skipping `iter_custom`. Revisit that selection against what the harness counts, beside [repeatable counts](/quest/m0/repeatable-instruction-counts.md).
- Kixel Quest finds the counts by the workflow's artifact and dispatches bisects by workflow. Check in kixel-ai/kixel.quest whether it keys on the file name `codspeed.yml` before renaming it, and move both together if it does.
- swole counts each CodSpeed bench five times, since it cannot see their threads, which took the Instructions workflow's full count from about 40 minutes to about 3 hours. A harness benchmark that runs alone counts once, so lower `codspeed.yml`'s `timeout-minutes` to fit the rewritten count.
- Update `bench/README.md`'s Instruction counts section and every comment that names CodSpeed or `cfg(codspeed)`.

Public API: none. Wire: none.

## Related

- [Repeatable instruction counts](/quest/m0/repeatable-instruction-counts.md) - the same counts' noise
