# [M] Add native adaptive playout targeting

## Goal

Native playback uses the shared measured-target algorithm and matches the browser
conformance corpus, without duplicating the existing audio sink and clock.

## Plan

On dev, decode::Config uses max_age and `moq play` already buffers audio to
--delay. Extend the current playout owner; a decoder-owned second buffer or a
new latency_min field is not a settled design. Before implementation, settle
floor versus fixed preset, estimator limits, and precedence with max_age.

Measure arrival at the same semantic point as the browser, apply the shared
corpus, and test format changes, tune-in, underrun/recovery, and A/V alignment
through real native output. Keep bounds on queued decoded memory. No time
stretching or loss concealment is included. Classify the public API impact
against the then-current release before choosing main or dev.

## Required

- [Spec](/quest/m2/audio-jitter-target/spec.md)
