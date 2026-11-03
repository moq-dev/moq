# [M] A shaped uplink proves native rung disable and resume

## Goal

One shaped-uplink run through moq-ffi proves the rung rule end to end over
real BBR3 probing: as the bottleneck narrows below a rung's band, the top
rendition stops and is advertised `enabled: false`, and once the bottleneck
opens back up it resumes with hysteresis, including after the grant fell below
the lowest rung. It runs nightly. The JS publisher is
[JS rung disable](/quest/m2/probe/rung-disable-js.md).

## Plan

The native rule and publishers have landed. The remaining transport proof is
staged in `rs/moq-ffi/src/test/rung.rs`, using the pre-encoded FFI path:
`MoqRateControl`, reservations, `MoqVideoInit.lower`, and
`MoqMediaTrackProducer::set_enabled`. `just rs rung-disable` runs the ignored
socket test and is wired into Nightly beside `quic-sim`.

The proof is not reliable yet. One real run completed the entire cycle, but a
fresh run disabled the top rendition during startup and never delivered it on
the initial wide link. It also stayed disabled after the path reopened. The
fixture drains both media cursors, retains its reservations, and keeps the
state assertions intact. Its final phase ends on observed recovery rather than
an exact recovery time; extending the hang guard did not settle the cold-start
failure.

Diagnose native probe recovery on the real driver/shaper path, both from cold
start and after a below-lowest grant. Probe traces show the target remains
installed and attempts run, but partial estimate gains do not reliably clear
the rung's resume margin. Small constant-rate deterministic QUIC spikes did
reach that margin, so the root cause is not isolated. No controller change or
instrumentation is part of this attempt.

Finish the proof at its source, pass local checks, and run `/quest-review`
before removing this quest and marking the PR ready. Keep nightly continuation
changes with [moq-dev/moq#7667](https://github.com/moq-dev/moq/pull/7667).

## Waiting

- moq-dev/moq: the full local gate must pass after the existing C/C++ pkg-config fix in [moq-dev/moq#7654](https://github.com/moq-dev/moq/pull/7654) and default Gradle discovery fix in [moq-dev/moq#7632](https://github.com/moq-dev/moq/pull/7632), on a Nix builder that permits user namespaces. The earlier gate also failed Dart's native-build freshness assertion; recheck it with the FFI build work in [moq-dev/moq#7666](https://github.com/moq-dev/moq/pull/7666), without importing those independent changes here.

## Related

- [JS rung disable](/quest/m2/probe/rung-disable-js.md) - the same rule in `js/publish`, probing with padding
- [Ladder controller](/quest/m3/ladder/controller.md) - the same rung disable for generated transcode ladders
- [Viewer up-switch](/quest/m2/probe/viewer-upswitch.md) - the viewer side of moving between renditions as capacity changes
