# [L] fMP4 import unwraps to Legacy

## Goal

First-party importers stop publishing CMAF unless asked. `moq import fmp4`
and `moq import hls` split each fragment into per-sample `Legacy` frames by
default. `--passthrough` keeps today's CMAF passthrough, which advertises
`Container::Cmaf` and sends one fragment per frame, mirroring
`import ts --passthrough`. Every other producer that defaults to CMAF flips
the same way.

CMAF makes each frame carry a `moof`, makes every player parse ISO BMFF,
and keeps `@svta/cml-iso-bmff` in the browser. Every other importer already
emits `Legacy`.

## Plan

Decided with the maintainer on 2026-10-20:

- The default lives in `moq-mux`, not just the CLI.
  `moq_mux::container::fmp4::Import` gains an unwrap mode and makes it the
  default. Passthrough becomes an explicit option, so moq-cli, moq-hls, the
  `ContainerFormat::Fmp4` path in `import/container.rs`, and FFI callers
  all get `Legacy` unless they opt in. Thread the opt-in through every path
  that builds an `fmp4::Import`: the importer itself (a builder like its
  `with_select`), `ContainerFormat` for `import/container.rs` and the FFI,
  and moq-hls's import args. Settle the exact shape with the maintainer in
  the PR. Breaking: add a `moq-mux` `MIGRATE.md` entry.
- Unwrapped output is `Legacy`, not LOC (maintainer choice). Each sample
  becomes one frame. Sample `i` is stamped `tfdt + sum(durations of samples
  before i) + composition_offset_i` in the track's timescale. The existing
  passthrough extraction in `fmp4/import.rs` already accumulates `dts` this
  way, so reuse it. The stamp is then published through the
  importer's `Timebase` like the passthrough path (`timebase.anchor`), so
  both modes land on the same catalog clock. Edit lists stay out of scope:
  both players already ignore `edts` today, and
  [fMP4 edit lists](/quest/m2/fmp4-edit-lists.md) applies them for both
  modes. Keyframes follow the
  `depends_on` rule moq-mux already applies ([moq-dev/moq#5078](https://github.com/moq-dev/moq/pull/5078)). The codec description
  comes from `stsd`.
- `moq import hls` takes the same `--passthrough`. HLS and fMP4 export
  already re-mux `Legacy` tracks through `fmp4::Muxer`. The `Legacy` wire
  carries no sample duration, so the muxer re-derives durations from codec
  data or the gap to the next frame, as it already does for TS, MKV and
  RTMP sources. A source whose exact `trun` timing must survive keeps
  `--passthrough`.
- Audit every CMAF producer and flip any that defaults to it: moq-gst's
  source, `moq-ffi`/`moq-c` media setup, `moq-video`/`moq-audio` helpers,
  and the demo and `justfile` recipes. Tests that need CMAF opt in
  explicitly.
- Update `doc/bin/cli.md`, `doc/bin/hls.md`, and every example invocation
  that relies on CMAF output, checked against `--help`.

## Related

- [Load CMAF decode on demand](/quest/m1/hang-cmaf-lazy.md) - the player side; this quest is what makes CMAF rare
- [fMP4 edit lists](/quest/m2/fmp4-edit-lists.md) - the unwrap path is where edit lists would apply to frame timestamps
- [CMAF surround Opus](/quest/m2/cmaf-opus-surround.md) - touches the same fMP4 import
