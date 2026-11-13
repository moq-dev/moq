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

Maintainer decision (2026-11-13): keep the quest and loop the full `--all`
matrix with `RUST_LOG=moq_mux=debug` until it reproduces, then fix the root
cause. Rejected: adding skip logging and parking the quest; closing it as not
reproducible.

### Reproduced 2026-11-13 ([#8611](https://github.com/moq-dev/moq/pull/8611))

Reproduced twice, both times as `js -> cpp`, so the publisher is not the cause:
once in the first full `--all` matrix under host-wide `stress-ng`, and once on
the 6th narrowed `--publishers js,python --subscribers cpp` run in a
`systemd-run --user --scope -p CPUQuota=50%` scope (5 runs at 150% passed).
Starve the test in its own cgroup, never the whole host.

Cause: the missing-sequence walk in `rs/moq-mux/src/container/consumer.rs`
(`if finished || elapsed || aged || excluded || full`), which until #8611 lost
the group without logging. The failing run logged
`skipping missing groups old=25 new=26 ... aged=true` and then
`audio gap after 11208 samples, at 1219266us for 40000us`. A starved
subscriber reads a burst of groups after a stall. Group 26 onward already
spans the playout target past the hole (moq-audio sets the wait to its target
via `set_wait`, tens of ms at `Delay.min` 0), so `aged` drops group 25 at once,
though its stream lands in the same burst. Every other subscriber decoded it.

Fix (waiting on the maintainer, see the PR): make the burst unable to age out
a hole on its own, recommended by requiring `elapsed` (wall time since a later
group arrived, `front.late`) for a missing sequence, so `aged` alone no longer
skips one. A backlog join with a true hole then waits one playout delay
(`late` is shared across the backlog, so not one per hole). Pin it with a
mocked-time consumer test: a later group buffered past the target, then the
missing group arriving before the wait, reads with no gap.
