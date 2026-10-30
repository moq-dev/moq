# Regressions

Failing inputs the fuzzer found, one file per input, in a directory named after its
target (`amf0/`). `fuzz::tests::regressions` replays them on the
stable toolchain as part of `just check`, allocation check included, so a bug found by
fuzzing stays fixed without anyone running the fuzzer again.

See [../README.md](../README.md) for the workflow.
