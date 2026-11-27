# [M] Transcoder honors lite-07 time starts, descending gap lookups, and live group skips

## Goal

`moq-transcode` rungs serve mid-group lite-07 time starts without dropping the containing source group, look up descending gaps in descending order, and declare live skipped spans via `skip_groups` so downstream subscribers and relays never stall or lose groups under zero-timer lite-07 accounting.

## Plan

[moq-dev/moq#9686](https://github.com/moq-dev/moq/pull/9686) added `output.finish_at(end)` in both `rung::live` and `rung::transcode_group` and removed local `TrackState::miss_grace`, leaving three lite-07 gaps in `rs/moq-transcode/src/rung.rs`:

1. **`rung::fetch` ignores `fill.is_time_start()` and `fill.floor()`:** `fetch` calls `source_track.fetch_group(sequence)` directly without checking whether `fill` is resolving a time start. When a time-start fill asks for timestamp `t` inside source group `[g, end)` (`g < t < end`), `fetch_group(t)` returns `NotFound`, `drop_gap` calls `fill.drop_groups(t..t+1)`, and per `rs/moq-net/src/model/track/fill.rs` dropping `t` on a time-start fill tells `moq-net` no group exists in `floor..=t`, skipping `g` completely. Update `fetch` to resolve time-start fills against the source's `[floor, t]` window (finding and accepting the containing source group `g` when one exists, and dropping only genuinely empty spans). Fix the `rung.rs` unit tests that currently manually accept the time-start fill before calling `spawn_fill`, which hid this bug.
2. **`drop_gap` scans ascending for descending fills:** `drop_gap` opens an `Order::Ascending` subscription over `first..=last` even when `fill.order()` is `Order::Descending`, walking and discarding every earlier group up to `sequence`. Subscribe `Order::Descending` from `sequence` for descending fills instead.
3. **`rung::live` never propagates skipped spans:** `live` publishes via `claim_group` and never calls `producer.skip_groups(...)` for empty spans before the first live group, after `Item::Lost`, or across source discontinuities where `next_start > prev_end`. Now that `TrackState::miss_grace` is gone, an undeclared gap on a local track stays owed until explicitly skipped or ended. Propagate skipped spans and discontinuities to `producer.skip_groups(...)`.

Add tests with realistic sparse timestamp-keyed groups (media-timescale keys rather than sequential 0, 1, 2) exercising `finish_at(end)`, `skip_groups`, unaccepted mid-group time-start fills, and descending gap drops.

Public API / wire: none (additive internal fix).
