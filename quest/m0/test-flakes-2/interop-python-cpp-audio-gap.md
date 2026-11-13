# [S] Interop's python -> cpp cell holds up under load

## Goal

`just test interop --all` passes the `python -> cpp` media cell on a loaded
host without it ending in `error: audio gap`.

## Plan

Planned 2026-11-13 under the repository's flaky-test rule, while iterating
[moq-dev/moq#8599](https://github.com/moq-dev/moq/pull/8599). One of three
`just test interop --all --publishers go,rust,python --subscribers cpp,js,js-native-node,rust`
runs on a shared, loaded host failed `python -> cpp` about 3 seconds in, the
cpp subscriber reporting `error: audio gap`; the other two runs passed, and
the logs did not show the grant close #8587 fixed. The failing run's logs are
gone.

Reproduce with `just test interop --publishers python --subscribers cpp` in a
loop under load, keeping the logs, and find whether the Python publisher
skips audio timestamps (a stalled encoder or a dropped group) or the cpp
subscriber's gap check misreads a legitimate group boundary; fix whichever
end is wrong, rather than widening the gap tolerance.

### Attempt 2026-11-12 ([#8611](https://github.com/moq-dev/moq/pull/8611))

No reproduction: 1 idle run plus 4 parallel runs of
`just test interop --publishers python --subscribers cpp` with `stress-ng --cpu $(nproc)`
all passed the cell in 1–2 s.

Ruled out by reading: the Python publisher can't skip audio stamps. `_publish_tone`
stamps every 20 ms batch at `timestamp_us += AUDIO_BATCH_US`, however late the
loop runs, so a stall only bursts writes. A `Gap` with contiguous stamps can only
come from `rs/moq-mux/src/container/consumer.rs` skipping a group: the max-delay
budget, a `Failure::Lost` group (aborted or evicted; it logs
`current group evicted`), or the byte cap moving the cursor past a stalled group.
Next: run the full `--all` matrix in a loop on a loaded host, and keep the cpp
subscriber's `RUST_LOG=moq_mux=debug` output so the skip reason
(`skipping slow groups` / `current group evicted`) shows up.
