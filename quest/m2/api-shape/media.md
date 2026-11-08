# [S] Media crates' small surface fixes

## Goal

Each item below has one path and one name:

- The container `Consumer` has one constructor, through
  `container::Playout`.
- `moq_mux::json::Options` folds into `json::Config`.
- `msf::Track.init_data` returns to `Option<String>`.

## Plan

Added 2026-11-07 (maintainer, /quest-plan from the 2026-11-07 API sweep of open PRs and remaining quests).

Found by the 2026-11-07 API shape audit (media formats); see the
[epic](/quest/m2/api-shape/README.md). The playout clock is
[mux-sync](/quest/m2/mux-sync.md).

The rest of this quest landed in its first PR: exporter `Delay`s, mp4-atom
kept out of moq-mux's API, echo `latency`, the JSON diff, `aac::Config`,
`Consumer::with_delay`, `ColorRange`, `Wall::MOQ_EPOCH`, `rate::Grant`, the
moq-stats re-exports, the FLV error, and the unused fMP4 and catalog
internals. Each remaining item conflicts with another rule, so it waits on
the maintainer.

## Waiting

- Maintainer: make `container::Consumer::new` crate-private so
  `Playout::consumer` is the only constructor? `new` inherits the delay the
  subscription already carries, which about 45 call sites in moq-audio,
  moq-cli, moq-gst, moq-ffi, and moq-c rely on; through `Playout` each would
  restate that delay or reset it to `Playout::default()`'s. Recommendation:
  keep `new` and drop this item, now that `with_delay` is gone.
- Maintainer: fold `json::Options` into `json::Config`? `Config` is the
  catalog entry that `json_stream` takes too, and `json_snapshot` accepts any
  `IntoRendition` entry, so `deltas` on it would be a setter streams ignore
  and an application section cannot carry. Recommendation: drop this item.
- Maintainer: return `msf::Track.init_data` to `Option<String>`? The
  `Arc<str>` is what
  [moq-dev/moq#7949](https://github.com/moq-dev/moq/pull/7949) added so a
  catalog referencing one large `initRef` from many tracks holds one copy; a
  `String` brings back the 0.6 MiB catalog that held 500 MiB.
  Recommendation: drop this item.
