# [M] Reduce audio stream opens with bounded groups

## Goal

Reduce audio stream-open overhead with bounded groups while retaining audio
on lite, IETF, and qmux/WebSocket paths. Preserve public APIs and catalog
semantics, and measure latency and loss behavior against one-frame groups.

## Plan

`js/publish/src/audio/encoder.ts` publishes each Opus frame as its own group.
At 20 ms per frame this opens about 50 streams per second. Establish whether
stream credit or stream-open work is a meaningful cost before changing it.

Compare short, duration-bounded groups against the current baseline. Forward
frames as they arrive rather than buffering an entire group before sending.
Close the final partial group on stop and start a new group on discontinuity
or configuration change. Bound group duration so late join and cancellation
do not depend on an indefinitely open stream. Preserve packet-loss concealment
metadata and the existing subscriber behavior at group boundaries.

Use reliable grouped delivery on every supported transport. Datagram delivery
is outside this quest: session support does not imply support at every hop or
subscriber, and the watch path does not currently consume datagrams.

Measure stream opens, stream-credit failures, dropped groups, encoder lag,
end-to-end latency, drift, late-join delay, and loss recovery under identical
clean and impaired workloads. Include simultaneous video and stalled readers.
Choose a group duration only from those comparisons; retain one-frame groups
if the overhead saving does not justify the latency or loss tradeoff.

Add CI regressions for final-group completion, discontinuity, late join,
reconnect, and audio delivery on stream-only sessions. Run smoke-full for the
cross-language delivery paths. Reuse the audio quality harness's metrics and
impairment fixtures when available rather than creating a second definition.

## Required

- [Browser benchmarks](/quest/m2/browser-benchmarks.md) - shared measurement and browser CI harness

## Related

- [Audio quality harness](/quest/m2/audio-quality-harness/browser.md) - audio measurements
