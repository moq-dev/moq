# Regressions

Failing inputs the fuzzer found, one file per input, in a directory named after its
target (`fmp4/`, `ts/`, `catalog/`, ...). `fuzz::tests::regressions` replays them on the
stable toolchain as part of `just check`, allocation bound included, so a bug found by
fuzzing stays fixed without anyone running the fuzzer again.

See [../README.md](../README.md) for the workflow.

## mp4-atom inputs

`cmaf/mp4-atom-{sgpd,iref,iloc}` preserve the minimized inputs from
[the fuzz findings](https://github.com/moq-dev/moq/pull/7313#issuecomment-6520535160),
including the two-byte init length. Their `fmp4/` counterparts replace that length
with a zero write-size selector and wrap the atoms in a `moov` box: streaming
ingest skips other top-level boxes. Both paths replay the malformed metadata with
the allocation bound enabled.

mp4-atom 0.17.1 contains the `sgpd` entry-count and zero-size `iloc` extent
bounds; 0.17.0 already fixed the `iref` subtraction overflow. moq-mux's metadata
preflight also bounds these inputs before calling the dependency.
