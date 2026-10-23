# [S] Nightly fuzzing keeps its corpus

## Goal

Each Nightly fuzz target starts from the corpus the previous run evolved, so
coverage found in one 300-second run compounds instead of being discarded
with the runner. A second run demonstrably loads the first run's discoveries.

## Plan

Decided 2026-10-22 from the repository improvement audit. Nightly runs twelve
targets for 300 s each on fresh hosted runners and uploads only crash
artifacts. `sh/rs/fuzz.sh` already keeps a writable
`rs/<crate>/fuzz/corpus/<target>` separate from generated seeds, so only the
workflow has to persist it.

- Store it in `actions/cache`: restore by a per-target key prefix, then save
  under a run-unique key after the run. Nightly runs daily, so the 7-day
  eviction never fires. Rejected: chaining artifacts between runs (needs
  cross-run lookup) and committing to a branch (needs a writer job with push
  credentials).
- Minimize with `cargo fuzz cmin` before saving, so the corpus stays bounded.
- Save only after a successful run; a crash input already reaches the
  failure artifact.
- Verify it: the second Nightly's log reports more initial corpus entries than
  seeds alone (libFuzzer's `INITED` line), for at least one target.
