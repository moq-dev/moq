# [M] Restore the replacement media case: audio across recurring gaps

## Goal

`just test media --media-cases replacement` passes every run on Chromium under CPU load, and the
default media run runs it again. Remove its quarantine in `test/interop/clients/js/media.ts` and
`test/interop/README.md`.

## Plan

The switch itself is fixed: the quest's PR found that after a fallback or replacement the audio
waited behind the incumbent's retained backlog, which the live picture had moved the shared clock
past, and the decode gate released that backlog one floor and window per worklet state report.
The gate now counts from where the ring skips to. Measured on a loaded host, the two builds
interleaved: before, 15 of 161 switches took 600 ms or more and one missed the 1039 ms bound; after,
1 of 165 did and none missed it.

What still fails under that load, at about one run in ten either way:

- A mid-stream gap restarts the audio codec and trims the catalog `warmup` (80 ms of Opus) from the
  first output after it (`js/watch/src/audio/terminal.ts` `update`, `rs/moq-audio/src/decode/consumer.rs`
  `warmup_pending`). One lost 20 ms group costs 100 ms of audio, and when the starved viewer skips a
  group more often than every 80 ms, as it does on a backlog read behind the clock, no output
  survives at all: one failing lite-06 fallback stayed silent (`audioTimestamp` 0) past its bound
  with every decoded output trimmed to zero frames. About 40% of the steady-state stalls in failing
  `audio tone` windows followed such a trim.
- The rest are underruns of the case's fixed 50 ms delay while the host is near 0% idle. The default
  run's other cases fail the same way at that load (`audio rendition switch back`, 1 of 4).

Recommended for the gap (a review can undo it): re-prime the restarted decoder on the encoded frames
just before the gap, restamped to end where the first frame after it starts, and drop their output,
so the warmup trims pre-roll rather than wanted audio. Chrome stamps an `AudioDecoder`'s output from
its first input, so the pre-roll must carry those restamped timestamps. moq-audio does the same, so
the native player keeps parity. Rejected: dropping the warmup trim after a gap reverses
[moq-dev/moq#5569](https://github.com/moq-dev/moq/pull/5569)'s decision to play only clean
output; keeping the decoder across a gap and restamping its output by input order assumes one
output per input, which AAC priming breaks.

Then rerun the replacement case on a loaded host and lift the quarantine if what fails is only the
starvation its sibling cases share. Measure with a loop like the PR's: `taskset` pins the run and
some busy loops to the same cores, so the load stays off other agents' cores.

Public API: none. Wire: none.
