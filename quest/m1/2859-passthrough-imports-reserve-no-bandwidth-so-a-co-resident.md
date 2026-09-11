# [L] Passthrough imports reserve their measured peak bitrate

## Goal

A passthrough import (`moq import rtmp`, `srt`, `rtc`, `hls`, or a container
on stdin) sharing a connection with a capture encoder claims each of its
tracks' peak-hold bitrate on the connection's allocator, so the encoder
targets what is left of the uplink instead of the whole of it. Passthrough
tracks reserve and never follow: nobody here chose their bitrate, so a grant
is nothing they can act on.

## Plan

Only encoders reserve today: the moq-video capture loop
(`rs/moq-video/src/encode/producer.rs:467-469`) and moq-audio's capture
driver (`rs/moq-audio/src/encode/capture.rs:471-474`). Passthrough tracks are
minted by `moq_mux::import::Track::{audio,video}`
(`rs/moq-mux/src/import/track.rs:195-202`, `:233-239`), which register
nothing, so a capture encoder over-targets by exactly the import's bitrate.
[#2809](https://github.com/moq-dev/moq/pull/2809) made running the two
together routine.

### The number

A passthrough track has no configured ceiling, and the allocator's rule is to
reserve a ceiling, never a measurement (`rs/moq-net/src/model/bandwidth.rs:213-218`).
`moq_mux::catalog::Estimate::bitrate` is the exception: it is the maximum
over 1 s windows, a peak-hold that only ever rises
(`rs/moq-mux/src/catalog/estimate.rs:6`, `:194-212`). Every container
`Producer` already keeps an `Estimator` (`rs/moq-mux/src/container/producer.rs:59`)
and the codec importers push it into the catalog rendition on every cut
(`rs/moq-mux/src/codec/legacy.rs:181-184`).

Update cadence: the container `Producer` calls `Reservation::update` whenever
`estimate().bitrate` exceeds the ceiling it reserved, which happens at most
once per closed window and only upward. That is legitimate under
`Reservation::update`'s contract (`bandwidth.rs:320-325`): the ceiling
genuinely moved. A ratchet discovers a ceiling late; it never follows a VBR
source down and never hands room away when the picture goes still.

Two known limits, neither fatal:

- The first window closes after 1 s of media, so until then the track claims
  nothing and a co-resident encoder over-targets by the difference. Take the
  reservation on the first estimate rather than reserving zero.
- The claim never shrinks over the life of the stream. That is the
  conservative direction.

### Plumbing

`spawn_import` receives the connection's allocator and discards it without
the capture feature (`rs/moq-cli/src/main.rs:448`, `:451-454`).
`crate::moq::ImportTarget { origin, name, max_age }`
(`rs/moq-cli/src/moq.rs:15-26`, minted at `main.rs:465-469`) gains
`bandwidth: moq_net::bandwidth::Allocator`, and every importer takes it the
way it takes `max_age`:

- rtmp and srt: `listen_import` / `connect_import` (`rs/moq-cli/src/rtmp.rs:47`,
  `:113`; `rs/moq-cli/src/srt.rs:35`, `:99`) hand it to
  `moq_rtmp`'s `Publish::accept` (`rs/moq-rtmp/src/server.rs:518-535`, into
  `Publisher::new`) and `moq_srt`'s (`rs/moq-srt/src/server.rs:333-337`,
  `serve_publish` `:414-418`) beside `with_max_age`.
- rtc: the same two entry points in `rs/moq-cli/src/rtc.rs:60`, `:88`.
- hls: `hls::import(origin, name, playlist, max_age)`
  (`rs/moq-cli/src/hls.rs:45-50`) takes an `ImportTarget` instead of the
  three loose fields and passes the allocator to
  `moq_hls::import::Import::new` (`rs/moq-hls/src/import.rs:613`).
- stdin containers: `Publish::new(broadcast, &format, max_age)`
  (`main.rs:476`, `rs/moq-cli/src/publish.rs:257`).

Each lands in `moq_mux::container::Producer`, which owns the track and the
`Estimator`, so the reservation sits beside the number that drives it. The
allocator rides the same constructor path `max_age` does; where that reaches
four arguments, fold the options into a struct.

Naming: `reserve` and `Reserved` in `rs/moq-mux/src/catalog/` are the catalog
gate that withholds the first snapshot (`rs/moq-mux/src/catalog/producer.rs:25`,
`:51`, `:360`; `rs/moq-mux/src/catalog/tracks.rs:283`). The bandwidth claim is
named explicitly, `bandwidth: Option<moq_net::bandwidth::Reservation>` and
`with_bandwidth(allocator)`, never a bare `reserve`.

Priority needs nothing: the importers stamp `hang::catalog::PRIORITY` per
kind (`track.rs:202`, `:239`; `rs/hang/src/catalog/priority.rs:21-26`), so an
imported audio track already outranks an imported video one.

Tests: next to `writes_measure_the_catalog_estimate`
(`rs/moq-mux/src/container/producer.rs:433`), a container `Producer` with an
allocator claims nothing before the first window, reserves the first
estimate, raises the ceiling on a larger later window and holds it on a
smaller one; an allocator test where a passthrough want at its peak lowers a
co-resident encoder's grant by exactly that amount.

Branch from dev.

## Closes

- [#2859](https://github.com/moq-dev/moq/issues/2859) - close this issue when the quest finishes

## Related

- [Binding rate control](/quest/m1/binding-rate-control.md) - the bindings follow the same allocator
- [Ladder](/quest/m2/ladder/README.md) - a transcode ladder dividing the same estimate
