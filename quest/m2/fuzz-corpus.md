# [S] Nightly fuzzing keeps its corpus

## Goal

Each Nightly fuzz target starts from the corpus the previous run evolved, so
coverage found in one 300-second run compounds instead of being discarded
with the runner. A second run demonstrably loads the first run's discoveries.

## Plan

The workflow restores per-target/platform corpus caches and saves a minimized
corpus under a run/attempt-unique key only after fuzzing and minimization succeed.
`just rs fuzz --minimize TARGET` minimizes only the evolved corpus; generated
seeds stay separate. Workflow and script regression tests pass, as does selected
CI.

A local real varint run initialized with 756 generated seed files at
`cov=156 ft=164 corp=30`, evolved to `cov=208 ft=217`, and minimized to 23
discovery files. After archiving, removing, and restoring that corpus, the next
run loaded 779 files and initialized at `cov=208 ft=217 corp=52` before further
exploration.

Remaining: verify the actual GitHub cache lifecycle over two Nightly fuzz job
executions. The second run must restore the first run's saved corpus and report
its discoveries in libFuzzer's `INITED` line. Keep all changes in fuzz tooling,
tests, and workflows; no production behavior or supported API changes are needed.
