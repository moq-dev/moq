# [M] Fix moq-mux, moq-archive, moq-json, hang ext, and moq-stats correctness bugs

## Goal

`mp4::repair`, AV1/fMP4 importers, `moq-archive` decoding/resume, `moq-json`
producers/consumers, `hang` ext serialization, and `moq-stats` on-demand track
reclamation all validate untrusted bounds and preserve state invariants under
error and concurrency paths.

## Plan

Decided 2026-11-21 (maintainer, /quest-plan of the exhaustive pre-cut
full-codebase audit): ✅ place high-severity and impactful correctness bugs in
`m1` (rejected: `m0` with `release` backports; `m3`).

1. **`rs/moq-mux/src/container/mp4.rs:320,354,382,593-598,778-785,838,944,978`**:
   - Enforce `fmp4::metadata` box/entry bounds in `mp4::repair` and reject
     `timescale == 0` so crafted MP4 inputs cannot OOM or divide by zero.
2. **`rs/moq-mux/src/codec/av1/split.rs:248-271`, `av1/mod.rs:96-107`,
   `import/track.rs:132,790`**:
   - Decode AV1 LEB128 in `u64` bounded to `<= u32::MAX` with `checked_add`
     (eliminates 32-bit/wasm32 shift panic).
   - Preserve `seq_tier_0` in `av1_from_av1c`.
   - Share one `is_av1c` (`len >= 4`) helper across `import/track.rs`.
3. **`rs/moq-mux/src/container/fmp4/{import.rs:990-1003,mod.rs:1142-1144,1165}`
   & `rs/moq-mux/src/catalog/msf/consumer.rs:485-496`**:
   - Read AAC sample rate and channel count from `esds` and Opus channel count
     from `dOps`, reject zero values, and use `try_from`/saturation instead of
     `as u16` / `as u8` silent wrapping (e.g., 96 kHz sample rate or 256 ch).
4. **`rs/moq-archive/src/layout.rs:126-128,292-317`, `consumer/track.rs:130`,
   `producer/track.rs:454-457`, `producer/resume.rs:31-36`**:
   - Validate `ID_MAX` in `End::decode` and `Object::validate(frame_start)`.
   - Use `checked_add` in `Live::end` and overflow-safe arithmetic in `units()`.
   - Scan the full resume object list in `producer/resume.rs` rather than
     stopping short.
5. **`rs/moq-json/src/{snapshot/producer.rs:547-553,window/producer.rs:394-400,stream/consumer.rs:25-34}`**:
   - Finish `previous` before calling `append_group` (or abort the newly
     appended group on error) so an error cannot strand an unfinished group.
   - Collapse `stream::Consumer`'s overlapping boolean flags into one explicit
     lifecycle enum.
6. **`rs/hang/src/catalog/root.rs:171-183`**:
   - Reject all reserved hang root keys in `serialize_ext`, not only `"version"`.
7. **`rs/moq-stats/src/produce.rs:1250-1263`**:
   - Fix the TOCTOU race when reclaiming on-demand tracks in
     `NodePublisher::serve_requests` so a newly subscribed track is not
     dropped concurrently with reclamation.

Public API: none. Wire: none. Add unit regression tests in each crate covering
the crafted/edge inputs and race conditions.
