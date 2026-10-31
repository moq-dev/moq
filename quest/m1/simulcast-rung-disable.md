# [M] OBS disables simulcast rungs the grant cannot fund, proven on a shaped uplink

## Goal

The OBS plugin stops sending its top video renditions when their bandwidth
grant cannot fund them, advertising each as `enabled: false`, and re-enables
them with hysteresis once the grant recovers, as the Rust and FFI publishers
already do. One shaped-uplink run through moq-ffi proves the rule end to end
over real BBR3 probing, including resuming a rung after the grant fell below
the lowest rung. The JS publisher is
[JS rung disable](/quest/m2/probe/rung-disable-js.md).

## Plan

Landed in [moq-dev/moq#6781](https://github.com/moq-dev/moq/pull/6781): the
rule lives in `moq_mux::rate` (`Policy::lower` sets the band boundary
`(max + 2 * lower) / 3`, `Follower` yields `rate::Target::Disabled`), only
once a session probes for the reservation (`bandwidth::Consumer::probed`).
An idle reservation reads the share it would get, so recovery does not depend
on demand. The FFI video producer takes `lower` (the next rendition down),
sits one track priority below it, drains its encoder while disabled, and
reopens on a keyframe; `moq_encode_video` takes the same `lower` handle in
its `moq_video_encode_options`.

- OBS: each OBS video encoder is its own rendition (`video_tracks` in
  `cpp/obs/src/moq-output.cpp`) publishing pre-encoded packets through a
  media track, and `MoQRate` (`cpp/obs/src/moq-rate.h`) mirrors
  `rate::Control` without the rung rule. It needs, through the generated C++
  package: a way to flip a media track's catalog `enabled`, the reservation's
  `probed` state (dropped from `MoqReservation` in #6781 for lack of a
  consumer), and the lower encoder's ceiling and priority. OBS cannot stop an
  encoder or force a keyframe per rung mid-stream, so decide between dropping
  packets while disabled and resuming at the next natural keyframe, or
  restarting the encoder.
- Shaped uplink: `moq-shaper` never steps its rate limit (queued datagrams
  would reorder), so a run whose bottleneck opens back up needs either a
  rate step the shaper can apply without reordering, or a second shaper the
  first retargets to. Run it nightly with an `#[ignore]` test and a recipe in
  `rs/justfile`, beside `quic-sim`.

## Related

- [JS rung disable](/quest/m2/probe/rung-disable-js.md) - the same rule in `js/publish`, probing with padding
- [Ladder controller](/quest/m3/ladder/controller.md) - the same rung disable for generated transcode ladders
- [Viewer up-switch](/quest/m2/probe/viewer-upswitch.md) - the viewer side of moving between renditions as capacity changes
