# [M] Stats names

## Goal

Each moq-stats track and reader has one name that says what it carries,
and "stats" means one thing in the CLI.

## Plan

Decided 2026-10-22 (API audit):

- `PREFIX = "traffic.json"` (`rs/moq-stats/src/lib.rs`), so `prefix_track()`
  returns `traffic.json` while `traffic_track()` returns
  `publisher.json`/`subscriber.json`. Rename the prefix track to
  `<path>/prefix.json`; update `doc/concept/stats.md` and the quests that
  cite `traffic.json` ([QoS](/quest/m2/qos/README.md),
  [lag dashboard](/quest/m2/qos-lag-dashboard.md),
  [lag histogram](/quest/m2/qos/lag-histogram.md)).
- `consume::{Traffic, Totals, Presence, Prefix}` share names with the frame
  types while `aggregate::` calls the same readers `*Consumer`; pick one
  scheme. Fix the doc's "Sessions" heading versus `Presence` and
  `FrameTooLarge` versus `FRAME_TOO_LARGE`.
- The five `*_track(.., compressed: bool)` functions and `COMPRESSED_SUFFIX`
  are left to [one compressed track per name](/quest/m1/compress/migrate.md),
  which deletes the `.z` siblings; no `Encoding` type (decided 2026-10-22,
  maintainer). Rename on top of its single-track functions.
- CLI: `import --stats` adds a media report, `export stats` prints it, and
  `--stats-*` publishes relay `.stats/`, which `export stats` cannot read.
  Rename the media surfaces `import --report` and `export report` (the code
  is `report.rs`), and drop the unused `--stats-*` flags from moq-cli.
  `stats.interval` becomes a typed duration and `--stats-linger` gets help.

moq.pro uses `traffic_track`, `sessions_track`, `COMPRESSED_SUFFIX`, and the
frame types; note each change in MIGRATE.

Public API: moq-stats and moq-cli breaking. Wire: the prefix track name
changes.

## Required

- [One compressed track per name](/quest/m1/compress/migrate.md) - drops the stats `.z` siblings and `TrackPair`, so names change once

## Related

- [Rust stats](/quest/m2/stats/rust.md) - builds on these CLI names
