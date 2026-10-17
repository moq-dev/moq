# Regressions

Crash inputs the fuzzer found, one file per input, in a directory named after its
target (`packet/`, `params/`, `streamid/`, `streams/`, `frame/`, `connection/`).
`fuzz::tests::regressions` replays them on the stable toolchain as part of `just check`,
so a bug found by fuzzing stays fixed without anyone running the fuzzer again.

See [../README.md](../README.md) for the workflow.
