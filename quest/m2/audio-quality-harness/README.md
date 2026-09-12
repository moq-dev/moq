# Audio quality harness

## Goal

A regression in audio playout latency fails a run instead of arriving as a bug
report. The harness plays a broadcast over an impaired path, counts what the
listener would actually have heard (underruns, short quanta, discarded samples,
skip-aheads) and what each stage of the pipeline contributed to the delay, and
grades the result against a checked-in budget. It runs in the browser and
natively, on the same jitter profiles, reporting the same numbers.

Boundaries: audio only. The stage breakdown is defined generically so video can
adopt it later, but no video assertion ships here. No perceptual scoring: the
grade is glitches and latency, not an opinion about how it sounds.

## Plan

Two quests, browser first, because that is where the traces and the reported
bug both are. The native lane follows against the same budgets and the same
metric schema, so the two implementations can be compared rather than merely
both passing.

The starting point is not a blank page. The reporter on #3477 already built a
working browser harness on their fork (`fperex/moq`, branch `debug/rt-audio`):
a CDP driver, a beacon sink, a trace analyzer, a ring replay, a five-scenario
`bench.sh`, and a `compare.mjs` that prints before-and-after tables, with 130
raw ndjson traces attached to release `rt-audio-traces-2026-09-06`. Upstream
that rather than reinventing it.

Jitter comes from the seeded userspace UDP shaper in [Impaired
path](/quest/m2/transport-impairment-profile.md), not from a fake arrival clock,
so the transport's own contribution is measured rather than assumed. That quest
builds the shaper inside the relay drills' support code; it has to come out into
something a JS harness process can also put in front of a relay. Extracting it
is part of the browser quest.

Loss, reorder and rate limiting stay switched off in the profiles here. The
buffer's job is absorbing arrival spread, and mixing congestion response into an
audio quality number makes a failure hard to attribute.

Nightly, not per-PR: the matrix is jitter profiles by runtime by codec and
sample rate, which is more than a merge gate should carry, and `nightly.yml`
already exists for exactly this trade. Budgets are keyed by the full row, since
each of those dimensions moves the expected floor.

## Quests

- [Browser](/quest/m2/audio-quality-harness/browser.md) - upstream the fork's harness, grade it against a budget, run it nightly
- [Native](/quest/m2/audio-quality-harness/native.md) - the same profiles and budgets through `moq play` on a dummy device

## Related

- [Audio jitter target](/quest/m2/audio-jitter-target/README.md) - the estimator this exists to keep honest
- [Latency ledger](/quest/m2/latency-ledger.md) - promotes this harness's probes into a public API
- [Failure artifacts](/quest/m2/qa-failure-artifacts.md) - keeps the run directory and trace of a failing run
- [Time stretch](/quest/m2/watch-audio-time-stretch.md) - graded by this harness once it lands
