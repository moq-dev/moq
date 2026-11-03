# [S] Finish validation of the shaped-uplink rung proof

## Goal

One shaped-uplink run through moq-ffi proves the rung rule end to end over
real BBR3 probing: as the bottleneck narrows below a rung's band, the top
rendition stops and is advertised `enabled: false`, and once the bottleneck
opens back up it resumes with hysteresis, including after the grant fell below
the lowest rung. It runs nightly. The JS publisher is
[JS rung disable](/quest/m2/probe/rung-disable-js.md).

## Plan

The native rule and publishers have landed. The remaining transport proof is
implemented in `rs/moq-ffi/src/test/rung.rs`, using the pre-encoded FFI path:
`MoqRateControl`, reservations, `MoqVideoInit.lower`, and
`MoqMediaTrackProducer::set_enabled`. `just rs rung-disable` runs the ignored
socket test and is wired into Nightly beside `quic-sim`.

The shaped run passed, including queue overflow, remote catalog flips, no
media timestamps in the disabled interval, a below-lowest grant, and a resumed
keyframe. The final phase ends on observed recovery, without imposing an exact
recovery time. Existing mock-grant tests cover the built-in encoder's teardown
and reopen. No production controller change is part of this proof.

Finish local validation and `/quest-review`, then remove this quest once the
full gate passes and the PR can be marked ready. Keep the nightly continuation
changes with [moq-dev/moq#7667](https://github.com/moq-dev/moq/pull/7667).

## Waiting

- moq-dev/moq: the full local gate must pass after the existing C/C++ pkg-config fix in [moq-dev/moq#7654](https://github.com/moq-dev/moq/pull/7654) and default Gradle discovery fix in [moq-dev/moq#7632](https://github.com/moq-dev/moq/pull/7632), on a Nix builder that permits user namespaces. The prior gate also failed Dart's native-build freshness assertion; recheck it with the FFI build work in [moq-dev/moq#7666](https://github.com/moq-dev/moq/pull/7666), without importing those independent changes here.

## Related

- [JS rung disable](/quest/m2/probe/rung-disable-js.md) - the same rule in `js/publish`, probing with padding
- [Ladder controller](/quest/m3/ladder/controller.md) - the same rung disable for generated transcode ladders
- [Viewer up-switch](/quest/m2/probe/viewer-upswitch.md) - the viewer side of moving between renditions as capacity changes
