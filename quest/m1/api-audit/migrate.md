# [S] MIGRATE matches release

## Goal

Every break already on `main` has a `MIGRATE.md` entry under `## Unreleased`
that is correct against `release`, not against an intermediate `main`.
Entries that describe APIs that never shipped are rewritten or deleted, and
packages that were never published carry no migration entries.

## Plan

Decided 2026-10-22 (API audit). Known gaps, verify each against
`git show origin/release:<path>`:

- Bindings: the synchronous shutdown request and awaited `closed()` result are
  synchronized by [the shutdown change](https://github.com/moq-dev/moq/pull/6436).
  Verify its entries against `release`, including the additive async `drain()`
  backport mapping, rather than documenting the intermediate async shutdown.
- Bindings: merge the contradictory `finish` entries ("removed (#4152)" and
  "the new `finish` lets go (#6157)") into one that names the final API:
  once [stateless broadcasts](/quest/m1/broadcast-lifecycle/stateless.md)
  lands, the broadcast `finish()` is gone, so a publisher finishes its tracks
  and closes the broadcast. If this lands first, leave the entry for that
  quest to rewrite rather than document the linger handle.
- Bindings: `rs/moq-ffi/MIGRATE.md` says a raw track is timed by default,
  but `MoqTrackInfo.timescale` defaults to `None` (untimed) while release
  defaulted to microseconds; the net change is timed to untimed by default.
- Bindings: `MoqAudioInit.track` and `MoqVideoInit.track` moved to
  `MoqMediaTarget::Named`, with no entry.
- `cpp/moq/MIGRATE.md`: about 130 lines for a package never released; empty
  it. The generated-binding MIGRATEs (`go/ffi`, `py/moq-ffi`, `kt/moq-ffi`,
  `swift/ffi`, `dart/moq_ffi`) list 4 of about 35 breaks plus checksum noise;
  point them at `rs/moq-ffi/MIGRATE.md` instead.
- `rs/moq-net`: `finish_at(Position)`, `final_sequence` renamed `end()`,
  `finished()` returning `Position`, and the transport error remapping
  (`IdleTimeout`, `StreamError::from(Transport)`) have no entries.
- `rs/hang`: the #5715 entries describe a `timelines` shape that never
  shipped; #4034 (`Range` to `Position`, `segment` to `sequence`,
  `DEFAULT_NAME` to `SUFFIX`) and JS #5288 (`Container.Consumer.next()`
  returns the `Read` union) have none.
- `rs/moq-archive`, `rs/moq-hls`: drop entries for unreleased intermediates
  (`Writer`, `Info::epoch`, `Config::timebase`); add `Key::Groups`,
  `Error::Bounds` to `Span`, `Rendition::segment(tag, n)`, and the segment URL
  change to `seg/{reference}.{n}.m4s`.
- `rs/moq-video`: `frame::macos` to `apple`, `vulkan::Channels` to `Format`,
  `Image` accessors becoming fields, `Handles::new` removed.
- `rs/moq-transcode`: `Rung::height` became `Rung::size`, meaning the short
  side. Rename the field `short_side` in the same PR so the entry names the
  final shape; moq.pro's `rs/transcode/src/ladder.rs` reads `.height`.
- `rs/moq-cli`: `--linger` worked on fmp4, mkv, flv, h264, and h265 on
  release and now refuses a nonzero value; iroh left the default features
  (#5581).
- `js/publish`: the "`Encoder.cut()` is not rate limited" entry describes an
  API added after release (#4184); `cut()` is additive.
- `js/clock`: was private on release, so its rename entries become a
  first-release note.

Docs only, apart from the `short_side` rename. Other children write their own
entries.
