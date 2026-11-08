# [M] Media crates' small surface fixes

## Goal

Each item below has one path and one name:

- moq-mux exporters (fmp4, mkv, flv, mp4): the fmp4 and catalog internals
  no caller uses become private, flv returns a typed error instead of a
  string, and fMP4, MP4, MKV, and FLV take a `moq_net::Delay { min, max }`
  (`min` the start behind the live edge, `max` today's skip budget), the
  pair [export-live-start](/quest/m3/export-live-start.md) needs, instead of
  one `Duration` under three spellings. TS keeps its jitter `delay`, a
  different role ([moq-dev/moq#7981](https://github.com/moq-dev/moq/pull/7981)).
- moq-mux stops re-exporting `mp4_atom` (`container::fmp4`), and
  `fmp4::Wire::{new, trak}` become `pub(crate)` or give way to the one
  accessor moq-hls needs, so an mp4-atom bump (two are planned in m3) is not
  a moq-mux break. The errors leak it too: `moq_mux::Error::Mp4` and
  `fmp4::Error::Mp4` wrap `mp4_atom::Error`, `fmp4::Error::{UnknownCodec,
  UnsupportedCodec}` carry `mp4_atom::FourCC` and `mp4_atom::Codec`, and
  both have `From<mp4_atom::Error>`. Those variants carry an opaque source
  and moq-mux's own codec name, and the public `From` impls go.
- hang's echo drops `latency` in Rust, JS, and the draft: nothing produces
  it, and [latency-ledger](/quest/m5/latency-ledger.md) adds its measurement
  additively.
- `moq_json::{diff, Diff}` become `pub(crate)`.
- `moq_mux::codec::aac::Config` gets `#[non_exhaustive]`, since
  [aac-sbr-config](/quest/m4/aac-sbr-config.md) adds a field.
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

Added 2026-11-07 (maintainer, /quest-plan from the 2026-11-07 API sweep of open PRs and remaining quests): exporter delays, mp4-atom, echo latency, the JSON diff, and the AAC config.

Found by the 2026-11-07 API shape audit (media formats); see the
[epic](/quest/m2/api-shape/README.md). The playout clock is
[mux-sync](/quest/m2/mux-sync.md).
Check the
`ts::Follower` and `ts::Release` collision too, and rename whichever name
the PR shows is ambiguous.
