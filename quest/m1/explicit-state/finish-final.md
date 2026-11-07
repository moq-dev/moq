# [M] A failed finish is final

## Goal

No media producer or importer can be left half-finished:

- A finish that fails leaves the object terminally failed and reports the
  cause. It never leaves the object marked finished with its tracks unended,
  and never leaves it usable with part of its pipeline torn down.
- Every container importer refuses input after finish in the same way.
- Where the type system can express it, finishing consumes the object, as
  the moq-rtmp and moq-srt publishers already do.

## Plan

The shared decisions are in the [epic](/quest/m1/explicit-state/README.md).

Bugs to reproduce and fix:

- **TS import** (`rs/moq-mux/src/container/ts/import.rs`): `finish` sets
  `finished = true` before the fallible `ensure!` checks, the reservation
  release, and `finish_stream` for each PID. `Drop` then never retries, so an
  input whose sections or private PES have no media timestamp leaves its
  streams unfinished. `decode` also never checks `finished`.
- **Audio encode** (`rs/moq-audio/src/encode/producer.rs`): `end()` takes the
  resampler before its fallible steps and clears `encoder` only at the end.
  After a failure, `write` still passes its `encoder.is_none()` guard and
  sends PCM at the input rate straight to the codec, and retrying `finish()`
  republishes the terminal packets.
- **JSON window** (`rs/moq-json/src/window/producer.rs`): `finish` sets
  `finished` before `track.finish()` can fail, and a retry then returns
  `Ok`. After `abort`, `ensure_open` reports `Closed` instead of the cause.
  The snapshot producer's design, `aborted: Option<Error>` plus openness read
  from the track, is the one to match.

Instances to sweep:

- Importer lifecycles (`mkv/import.rs`, `fmp4/import.rs`, `ts/import.rs`):
  MKV refuses `decode` after finish, but fMP4 and TS don't. MKV also overloads
  `initial_reservation == None` with three meanings. Recommended: a shared
  `enum Input { Open, Failed(Error), Finished }`.
- `rs/moq-mux/src/container/producer.rs`: `close()` resets the group's
  fields only after its fallible steps.
- `rs/moq-transcode/src/rung.rs`: `pending_cut` is taken before the cut that
  can fail.
- The moq-json `Track.group` invariant: clearing it must also reset the
  encoder.

The importer and producer `finish` signatures are public API. A move to
`finish(self)` is breaking, so if this child takes it, it leaves the epic and
becomes its own m2 quest under the epic's rule.
