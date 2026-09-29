# Audio quality harness

## Goal

A regression in audio playout latency fails a run instead of arriving as a bug
report. The harness plays a broadcast over an impaired path, counts what the
listener would actually have heard (underruns, short quanta, discarded samples,
skip-aheads) and what each stage of the pipeline contributed to the delay, and
grades the result against a checked-in budget. This line is the browser;
the native lane is [Native audio
quality](/quest/m1/audio-quality-native.md), on the same jitter profiles and
reporting the same numbers.

Boundaries: audio only. The stage breakdown is defined generically so video can
adopt it later, but no video assertion ships here. No perceptual scoring: the
grade is glitches and latency, not an opinion about how it sounds.

## Plan

Browser only, because that is where the traces and the reported bug both
are. The native lane moved to m1 as a standalone quest (decided in the
2026-09-28 quest audit): nothing in m0 waits on it, and it follows against the
same budgets and metric schema so the two implementations can be compared
rather than merely both passing.

The browser lane has landed as `test/audio-quality/`, upstreamed from the
reporter's fork on #3477 (`fperex/moq`, branch `debug-findings-solution`) on
its own, without the fork's player, estimator, or shaper changes. Real arrival
traces have landed beside it in `test/audio-quality/traces/`, graded as replay
rows through the player's container consumer and rings, and replayed by the
[jitter target's watch quest](/quest/m0/audio-jitter-target/watch.md). That
leaves the bursty and step profiles, which need shaper features the fork has
and `main` does not.

Jitter comes from the seeded userspace UDP shaper the transport drills run
under (`rs/moq-shaper`, documented in `test/drill/README.md`), not from a fake
arrival clock, so the transport's own contribution is measured rather than
assumed. Its `moq-shaper` binary is what a JS harness process puts in front of
a relay.

Loss, reorder and rate limiting stay switched off in the profiles here. The
buffer's job is absorbing arrival spread, and mixing congestion response into an
audio quality number makes a failure hard to attribute.

Nightly, not per-PR: the matrix is jitter profiles by runtime by codec and
sample rate, which is more than a merge gate should carry, and `nightly.yml`
already exists for exactly this trade. The recorded-trace replays are the
exception: they run in seconds without a browser, so
`.github/workflows/audio-quality.yml` gates every PR touching the player's
packages or the harness on them. Budgets are keyed by the full row, since
each of those dimensions moves the expected floor.

The budgets checked in with the browser lane (#4426) were recorded locally.
Once the line lands, re-record `test/audio-quality/budgets.json` from the
nightly runner's first runs, since nightly only runs `main`'s code. Tightening
the auto rows belongs to the [jitter target's watch
quest](/quest/m0/audio-jitter-target/watch.md).

## Required

- [Shaper profiles](/quest/m0/audio-quality-harness/shaper-profiles.md) - the bursty and mid-run step profiles, once moq-shaper can express them

## Related

- [Native audio quality](/quest/m1/audio-quality-native.md) - the same profiles and budgets through `moq play` on a dummy device
- [Audio jitter target](/quest/m0/audio-jitter-target/README.md) - the estimator this exists to keep honest
- [Latency ledger](/quest/m2/latency-ledger.md) - promotes this harness's probes into a public API
- [Time stretch](/quest/m1/watch-audio-time-stretch.md) - graded by this harness once it lands
