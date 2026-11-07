# [M] Media crates' small surface fixes

## Goal

Each item below has one path and one name:

- moq-mux exporters (fmp4, mkv, flv, mp4): the fmp4 and catalog internals
  no caller uses become private, flv returns a typed error instead of a
  string, and every exporter names its wait the same way (the name moq-mux's
  TS exporter uses since [moq-dev/moq#7981](https://github.com/moq-dev/moq/pull/7981)).
- The container `Consumer` has one constructor, through
  `container::Playout`; `with_delay` goes.
- hang's `catalog::Range` becomes `ColorRange`, and the MoQ epoch constant
  lives only in `kio::clock::Wall`; hang and moq-mux re-export nothing.
- `moq_mux::rate::Follower` becomes `rate::Grant`, so it no longer collides
  with `ts::Follower`.
- moq-stats drops its ten `moq_net::stats` re-exports.
- `moq_mux::json::Options` folds into `json::Config`.
- `msf::Track.init_data` returns to `Option<String>`.

## Plan

Found by the 2026-11-07 API shape audit (media formats); see the
[epic](/quest/m2/api-shape/README.md). The playout clock is
[mux-sync](/quest/m2/mux-sync.md), and rendition stats are
[rendition-stats-type](/quest/m2/rendition-stats-type.md). Check the
`ts::Follower` and `ts::Release` collision too, and rename whichever name
the PR shows is ambiguous.
