# [L] Define native encoder latency and quality presets

## Goal

moq-video/moq-audio provide Low latency (default), Balanced, and Quality policies usable by OBS, with bitrate configured independently and actual applied controls reported.

## Plan

- Put the policy in codec configuration rather than copying backend settings into the OBS UI. Use a small extensible preset enum/options field with consumer documentation. Backends retain responsibility for mapping policy to supported controls.
- Keep frame reordering disabled across the initial video presets. Low latency minimizes supported buffering; Balanced and Quality spend additional codec effort on compression where supported without unbounded queues. Several existing backends hardcode low-delay settings, so report equivalent mappings honestly instead of manufacturing delay or pretending every backend has three distinct modes.
- Start Opus measurement with 10 ms packets for Low latency and 20 ms for Balanced/Quality, varying supported codec effort only where an API exists. Treat these as packetization settings, not guaranteed delay. Do not use 2.5 ms through an integer-millisecond C API. Preserve explicit bitrate and channel settings.
- Define bounded input/output ownership and full-queue behavior. Drop unsubmitted raw video when saturated, not arbitrary interdependent encoded packets. Separate encoder delay, keyframe join time, transport delay, and viewer playout. Do not use the consumer's stalled-group skip ceiling as an encoder preset.
- Measure p50/p95 frame-to-packet latency, throughput, queue depth, and quality at matched bitrate/resolution on supported backends. Add tests showing requested policy reaches the backend and Stats reports controls that actually succeeded. Resolve ignored low-latency property failures before claiming a policy is active. Publish the measured mappings in OBS docs without promising a fixed end-to-end target.
