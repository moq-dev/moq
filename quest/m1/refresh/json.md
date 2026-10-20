# [M] moq-json groups roll on demand

## Goal

moq-json and `@moq/json` snapshot and window producers roll a new group only
when a joiner wants one, or at the existing hard caps, instead of whenever
deltas reach `delta_ratio` times the snapshot (or ops reach `op_ratio`
times the window). The ratio comes from the track's `track::Info::refresh`.
`delta_ratio` and `op_ratio` are removed in Rust, JS, moq-ffi, and every
binding, and a `deltas` / `ops` switch keeps today's no-delta mode.

## Plan

Decided 2026-10-16 (see the [epic](/quest/m1/refresh/README.md)), and
revised in the #5734 review.

- **The ratio lives only on `track::Info::refresh`.** The producers receive
  an already-created track, and `track::Info` is fixed at `create_track`,
  so a json config field could never reach TRACK_INFO. The producers read
  `track.info().refresh` instead. Constructors do not change, and there is
  one source of truth. The default is therefore `None`: groups roll only at
  the caps unless the caller sets it. Docs, examples, and in-repo callers
  set `Some(8)`, today's `delta_ratio` default.
- **No-delta mode.** `delta_ratio = 0` and `op_ratio = 0` mean "never send
  deltas or ops" today, which the catalog producers and moq-stats' plain
  track rely on. That becomes `snapshot::Config::deltas: bool` and
  `window::ProducerConfig::ops: bool` (JS `deltas` / `ops`), default true.
  False makes every update its own snapshot group, and refresh is moot.
  Rust `with_delta_ratio` and `with_op_ratio` and JS `deltaRatio` and
  `opRatio` go away.
- **Producers.** When `Demand::wanted` passes the largest group, the
  producer re-emits the current value as a snapshot (or the current window)
  in a new group immediately, rather than waiting for the next update.
- **Driver.** The producers are synchronous and moq-json has no runtime,
  so the Rust caller drives refresh: `Producer::refresh(&self).await` (plus
  `poll_refresh` for sync owners) waits until `wanted` passes the largest
  group, then re-emits. Callers loop or select on it beside their updates.
  JS producers drive it internally. Decided in the #5734 review, over a
  driver handle returned by the constructor and over checking only on
  update, which never refreshes an idle value.
- **Driver (moq-ffi).** Each bound json producer drives refresh
  internally, as JS does: its constructor starts a task with
  `crate::ffi::spawn` that waits on the track's demand, re-emits the value
  under the producer's lock, and stops on finish or drop. Bindings gain no
  refresh API. Decided in the #5734 review over exposing an async
  `refresh()` in moq-ffi and every wrapper (about 150 lines), which every
  binding caller would have to loop on, and a missed loop fails silently.
- **Timestamp.** Producers and encoders retain the timestamp of the last
  committed update, and a refresh restates the value at it. That is the
  state's own time and never rewinds, because it equals the previous
  group's last frame. Untimed tracks restate untimed.
- **Track-free encoders.** `snapshot::Encoder` (and the window encoder, JS
  too) gain a `snapshot()` operation that re-encodes the retained value as a
  snapshot now, for owners that open groups themselves. moq-stats moves to
  `snapshot::Producer` where its shared-sequence needs allow, and otherwise
  watches `Demand::wanted` and calls `Encoder::snapshot()`. The maintainer
  asked for both.
- **Caps.** The unconditional budget roll goes away. `MAX_DELTA_FRAMES` and
  the group byte cap still force a snapshot. They are the bound behind a hop
  that cannot forward requests, so a joiner there downloads at most up to
  the caps.
- This works locally without wire support: a local subscriber's join
  drives `wanted` through the track model alone.
- **In-repo users follow:** the hang and moq-mux catalog producers
  (`deltas: false`), `js/publish` catalog, moq-stats, the moq-room and
  `js/room` chat windows, the moq-hls and moq-archive tests, benches, and
  examples. The tracks they create set `track::Info::refresh` where they
  keep deltas.
- **Bindings.** Replace `delta_ratio` with `deltas` in moq-ffi
  (`rs/moq-ffi/src/json.rs`), moq-c, the py, swift, go, kt, and dart wrappers, and
  `doc/lib/*`. The ratio reaches them through `MoqTrackInfo::refresh`, and
  the moq-ffi driver above refreshes
  their producers with no wrapper change. Update the moq-json and `@moq/json`
  READMEs and add an entry to each affected package's `MIGRATE.md`.
- moq-flate is unchanged. Its snapshot mode already opens a group per
  update, and its stream mode keeps one group by design.

Tests: under simulated time, a joiner below the ratio gets the current
group, and a joiner above it gets a fresh snapshot group. With no joiner the
group grows to the caps. `refresh: None` never rolls early, `deltas: false`
writes one snapshot group per update, and `Encoder::snapshot()` restates the
retained value. A refresh on a timed track reuses the last committed
timestamp. Run in Rust and JS. A moq-ffi test shows an idle bound
snapshot producer re-emitting its value when a joiner crosses the ratio.

Public API: breaking (`delta_ratio` and `op_ratio` replaced by `deltas` and
`ops` in Rust, JS, and every binding that exposes them; `Encoder::snapshot()`
and Rust `Producer::refresh` / `poll_refresh` added; bindings gain no refresh
method because moq-ffi drives it internally). Wire: none.

## Related

- [JSON window timestamps](/quest/m1/json-window-timestamps.md) - touches the same window code
