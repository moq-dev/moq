# [M] PCM batching avoids repeated tail movement

## Goal

Encoding and resampling larger PCM batches does not repeatedly move the
same remaining samples, while preserving audio and timestamp behavior.

## Plan

[Producer::publish_full_frames](/rs/moq-audio/src/encode/producer.rs)
front-drains into a new Vec for each packet after appending the entire input
(lines 379-389). [Resampler::convert](/rs/moq-audio/src/resample.rs) likewise
front-drains after every chunk (line 291). N chunks in a call can move a
triangular number of trailing samples. Capture and binding producers use these
paths; small normal capture periods may hide the cost.

Benchmark a constant duration of audio delivered in different batch sizes,
channels and input/output rate pairs, also sweeping independent producers.
Separate buffering/resampling from codec CPU with a cheap supported codec
control. Measure instructions/sample, allocations, copied bytes, throughput
and worst call duration, including tiny steady batches and large bursts.
Verify PCM or decoded output as appropriate, sample counts, packet boundaries,
timestamps, partial tails, flush and discontinuities.

Processing slices and compacting only the remainder is a candidate; do not
prescribe a queue type before measuring. This does not change capture period
policy or the [Opus grant](/quest/m2/2848-follow-the-bandwidth-grant-in-moq-audio-instead-of.md).

Follow the [survey's benchmark contract](/quest/m2/performance-survey/README.md):
measure the baseline before optimizing, retain paired evidence, verify equivalent
work, schedule coverage nightly, and accept a measured no-win. Expected public
API and wire impact: none; report the actual impact in the implementation PR.
