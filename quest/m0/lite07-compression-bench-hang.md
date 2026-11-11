# [M] session_delivery_compression finishes on moq-lite-07

## Goal

The `session_delivery_compression` Criterion group (moq-net's `session`
bench target, `rs/moq-net/benches/session/mod.rs`) finishes every
`moq-lite-07-wip` cell, and the root cause of the hang it shows is fixed. If
the hang is in moq-net rather than the benchmark harness, a test reproduces it
in the merge gate.

## Plan

`Nightly (Rust bench targets)` was cancelled at its 60-minute timeout on the
2026-11-02, 11-05, 11-06, 11-09, and 11-10 scheduled runs, and failed without
timing out on 11-03, 11-04, 11-07, and 11-08.
In [the 11-10 run](https://github.com/moq-dev/moq/actions/runs/42842827718/job/129922278472)
every `moq-lite-06` cell finished, then
`session_delivery_compression/moq-lite-07-wip/relays=1/publishers=1/broadcasts=1/viewers=1`
printed `Benchmarking` and nothing for the job's last 34 minutes, so the hang
starts with the first lite-07 cell, and every later bench in the job goes
unmeasured. Reproduce it locally, bisect against the lite-07 changes
if it is a regression, and fix it at the root.

## Related

- [JS subscribe-drop finishes](/quest/m0/js-subscribe-drop-hang.md) - another Nightly benchmark that hangs
