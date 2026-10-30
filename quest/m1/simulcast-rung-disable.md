# [M] A shaped uplink proves native rung disable and resume

## Goal

One shaped-uplink run through moq-ffi proves the rung rule end to end over
real BBR3 probing: as the bottleneck narrows below a rung's band, the top
rendition stops and is advertised `enabled: false`, and once the bottleneck
opens back up it resumes with hysteresis, including after the grant fell below
the lowest rung. It runs nightly. The JS publisher is
[JS rung disable](/quest/m2/probe/rung-disable-js.md).

## Plan

The rule and every native publisher landed: `moq_mux::rate` in
[moq-dev/moq#6781](https://github.com/moq-dev/moq/pull/6781) (`Policy::lower`
sets the band boundary `(max + 2 * lower) / 3`; a rung disables only while a
session probes for its reservation), the FFI and C video producers there too,
and OBS through moq-ffi's `MoqRateControl`, `MoqVideoInit.lower`, and
`MoqMediaTrackProducer::set_enabled`, which a pre-encoded publisher follows
the same way.

- `moq-shaper` never steps its rate limit (queued datagrams would reorder),
  so a run whose bottleneck opens back up needs either a rate step the shaper
  can apply without reordering, or a second shaper the first retargets to.
- Shape it as bufferbloat: a rate limit below the top rung behind a deep but
  finite queue whose overflow drops packets. Assert the disable, the catalog
  `enabled` flips, and the resume, not exact timings.
- Run it nightly with an `#[ignore]` test and a recipe in `rs/justfile`,
  beside `quic-sim`.

## Related

- [JS rung disable](/quest/m2/probe/rung-disable-js.md) - the same rule in `js/publish`, probing with padding
- [Ladder controller](/quest/m3/ladder/controller.md) - the same rung disable for generated transcode ladders
- [OBS multitrack](/quest/m2/obs-multitrack.md) - OBS's native multitrack encoders as the renditions this disables
- [Viewer up-switch](/quest/m2/probe/viewer-upswitch.md) - the viewer side of moving between renditions as capacity changes
