# [L] Key groups by frame 0 and remove group timestamps from producer APIs

## Goal

Every published group is created eagerly from its first frame (no separate group timestamp and no empty groups) across Rust (`moq-net`), JS (`@moq/net`), `moq-ffi`, `libmoq`, and all language bindings, so `group.start == frame[0].timestamp` holds by construction:

- `moq-lite-07` drops the first `FRAME`'s `Timestamp Delta` when `Frame Start == 0`, uses unsigned deltas for every other `FRAME` (including `Frame Start > 0`'s first frame), removes zigzag from `lite-07`, and treats zero frames before `GROUP_END` as malformed only when `Frame Start == 0`. A resumed slice with `Frame Start > 0` and zero frames before `GROUP_END` (e.g. resuming at `Frame Start = N` after reading all `N` frames to learn the group's end) remains valid, with `End Delta` measured from `Group Start`. `GROUP_END` (`End Delta`) stays as the last frame's exclusive end.
- `moq-transport` draft-23 (`moq-timestamp-03`) has frame 0 at `Object ID = 0` with `TIMESTAMP_TIE = 0`; a received group whose first object misses `Group ID` is refused (`TimestampMismatch`).
- Legacy ingress (`lite-05/06` and the `draft-18..22` shim) re-keys timed groups by frame 0's timestamp on arrival; empty legacy groups are dropped. Unsupported by-key history requests against re-keyed legacy tracks are rejected before consulting the cache, so warm and cold relays behave identically (only live subscriptions bridge across them, and a relay never forwards timestamp keys as legacy sequence numbers).
- `moq-mux`, `moq-ffi`, `libmoq`, and bindings remove `seek(start)`.
- `track::Producer::drop_groups(range, err)` is public in Rust and JS so replay producers (such as `moq-archive`) report missing or pruned group keys without creating empty groups.
- Wire and cache group keys (`Group Start` / `Group ID`) stay for subscriptions, drops, skips, `GROUP_END` spans, and `Frame Start > 0` slices.

## Plan

Decided 2026-11-27 (maintainer, `/quest-plan` following the frame-0 design pass on [moq-dev/moq#9794](https://github.com/moq-dev/moq/pull/9794)):

- ✅ Split into its own `m2` quest after timestamp-ids ([moq-dev/moq#9794](https://github.com/moq-dev/moq/pull/9794)) and blocking [lite07-finalize](/quest/m2/lite07-finalize.md), letting #9794 finish review and land with its temporary zigzag first `FRAME` delta on `lite-07-wip` (rejected: folding into #9794, which would collide with its active branch and reset three review rounds; dropping, which would freeze the separate group-start API and zigzag first delta into 0.22 and lite-07).
- ✅ Keep `GROUP_END` (`End Delta` / `finish_at`) as the last frame's exclusive end: duration is not a frame property because that adds a frame of latency, and `GROUP_END` carries the last frame's end for exporters, gap coverage, and final-group expiry.
- ✅ Eager first-frame group creation across Rust, JS, `moq-ffi`, `libmoq`, and all bindings, unified for timed and scale-less tracks since scale-less frames already carry `Timestamp::unscaled(key)` (rejected: lazy uncommitted `append_group()`, which turns `group.start` into `Option` and adds a pre-frame-0 error state to `consume`, `ready`, `finish`, and `finish_at`; splitting creation APIs into timed and untimed forms).
- ✅ Remove `seek(start)` from `moq-mux`, `moq-ffi`, `libmoq`, and all bindings (rejected: keeping `seek(start)` as a minimum-start floor or unskipped-gap helper). `seek` was a sequence-number relic that explicitly decoupled group keys from frame 0; `cut` draws group boundaries on track/codec importers and fMP4 (`Container::cut` is a no-op on MKV, TS, and FLV, which group on container keyframes), and tests needing an unskipped gap write via `track::Producer::create_group`.
- ✅ Expose `track::Producer::drop_groups(range, err)` in Rust and JS, mirroring `skip_groups` (rejected: keeping `drop_groups` private and having `moq-archive` report pruned or corrupted groups as clean `skip_groups`). Bindings add `drop_groups` only if a binding consumer needs it.
- ✅ Legacy ingress (`lite-05/06` and the `draft-18..22` shim) re-keys timed groups by frame 0's timestamp on arrival, explicitly accepting that by-sequence requests break (recorded as Decision C on [moq-dev/moq#9794](https://github.com/moq-dev/moq/pull/9794) on 2026-11-27; rejected: leaving older-version copies unchecked with sequence keys). Empty legacy groups carry no frame 0 and are ignored, and unsupported by-key history requests for re-keyed legacy tracks are rejected before cache lookup (preserving the cache invariant so warm and cold relays give the same answer instead of only rejecting upstream fills).

### Target public API

- **Rust (`moq-net`):**
  - `track::Producer::append_group(timestamp, data) -> Result<group::Producer>` (in-order, whole frame 0)
  - `track::Producer::create_group(timestamp, data) -> Result<group::Producer>` and `claim_group(timestamp, data) -> Result<Option<group::Producer>>` (out-of-order, whole frame 0)
  - `track::Producer::append_frame(info: frame::Info) -> Result<frame::Producer>` and `create_frame(info: frame::Info) -> Result<frame::Producer>` (streamed frame 0; `frame::Producer::finish` already returns `Result<group::Producer>`)
  - `track::Producer::write_frame(timestamp, data) -> Result<()>` (unchanged: single-frame finished group)
  - `track::Producer::drop_groups(groups: Range<u64>, err: StreamError)` (promoted from `pub(crate)`)
  - `track::Producer::has_group(start: u64) -> bool` (read-only check so `moq-transcode` `rung.rs` can still skip GPU encoding when a fetch already claimed a GOP)
- **JS (`@moq/net`):**
  - `TrackProducer.appendGroup(frame: Frame): GroupProducer` and `createGroup(frame: Frame): GroupProducer`
  - `TrackProducer.appendFrame(info: FrameInfo): FrameProducer` and `createFrame(info: FrameInfo): FrameProducer` (`FrameProducer.finish()` returns `GroupProducer`)
  - `TrackProducer.writeFrame(frame: Frame): void` (unchanged)
  - `TrackProducer.dropGroups(start: bigint, end: bigint, error: Error): void`
- **`moq-ffi` (Swift, Kotlin, Go, Python, Dart, C++):**
  - `append_group(frame: MoqFrame)` and `create_group(frame: MoqFrame)` (same `MoqFrame` as `write_frame`)
  - Remove `seek` from media/container importers and every language wrapper.
- **`libmoq` (C ABI):**
  - `moq_publish_track_group(track, payload, payload_size, timestamp) -> i32`
  - `moq_publish_track_create_group(track, payload, payload_size, timestamp) -> i32` (replaces `moq_publish_track_group_at`)
  - Remove `seek` from the C media/container publish API.

### Internal simplifications and areas to update

- Collapse the separate `group::Info.start` vs. lazy `Timeline::Timed.start` / `Neighbors` / `mark_started` / `reveal_started` machinery in `rs/moq-net/src/model/` (and `#admits` / `started` in `js/net/src/group.ts`) where frame 0 now settles the start on creation. Keep `pub(crate)` wire-receive constructors (`create_group_at`) internal for received `GROUP` headers and `Frame Start > 0` slices.
- Update `moq-mux` (`fmp4/import.rs` uses `append_frame` for streamed frame 0; `container/producer.rs` and codec/container importers drop `seek`), `moq-audio` warmup/join tests (replace `producer.seek(...)` with direct `create_group`), `moq-archive` (use `drop_groups` for pruned/unusable groups), `moq-transcode` (`rung.rs` uses `has_group` + `claim_group`), `moq-json` / `js/json` (remove post-`append_group` rollback glue), and tests across the workspace (the earlier model check hit ~486 tests across 14 crates that keyed groups by sequence while stamping unrelated frame-0 times).
- Update `drafts/draft-lcurley-moq-lite.md` (`GROUP`, `FRAME`, `GROUP_END`, and changelog), `drafts/draft-lcurley-moq-timestamp.md`, `doc/concept/moq-lite.md`, and run `just check` and `just test interop --all`.
