# [M] moq-transport range updates

## Goal

A relay carries a downstream range change to its moq-transport upstream in the
subscription update that already carries the aggregate priority, widening
included, and resolves a `Time(pts)` end before it goes upstream.

## Plan

The implementation is published in draft [moq-dev/moq#6815](https://github.com/moq-dev/moq/pull/6815).
It carries aggregate range changes in the existing `write_update` path from
draft 15 onward, preserving draft 14's limitation. Draft-22 widening requests
fills in ascending order; the publisher serves multiple groups and owns the
current-group tail separately from its history fill. Cancellation releases a
blocked fill without ending the subscription's other groups.

Cached `Time(pts)` ends resolve to group bounds before going upstream, or are
refused without replacing the previous requested ranges. Fetch-only copies
retain the exact upstream Largest Object independently of their cache. Empty
ranges retire the wire subscription when no sibling holds demand, and can
widen again using the retained copy.

Standalone FETCH still serves the whole Largest Object group after it
finishes (maintainer, 2026-10-25, departing from draft-22 section 9.11). The
cache cannot know an open group's real Largest Object, so it must not hand a
downstream cache a truncated group as complete. Joining fills keep their
snapshot-head and live-tail split.

Source checkpoint [4e9050af2302](https://github.com/moq-dev/moq/commit/4e9050af23026478749bc2f963fd6e13eca7e84a) includes main [011402e005](https://github.com/moq-dev/moq/commit/011402e005f6c8efeff07c46535fa2cfa95f021e), the Rust range,
anchor, and lifecycle implementation, matching JavaScript fill-order codecs,
and a session benchmark over groups per fill and concurrent requests. No
maintainer decision is currently identified; this quest remains claimed while
validation is unfinished.

Before merging main, the full moq-net suite passed 3,227 of 3,228 tests, with
11 skipped. The sole failure was draft-14 unsupported Time-anchor error
precedence, corrected in [de20deea09](https://github.com/moq-dev/moq/commit/de20deea093709f327952b88d0d4996b47ccf3c9) without weakening its assertion. The run
passed supported legacy narrowing/widening, draft-22 repeated bound changes,
quiet-head stitching, and the strict cached-Time refusal guarantee. The
JavaScript helper passed 2,746 tests, TypeScript, and Biome checks.
Merged-head Rustfmt, changed-file Biome, and whitespace checks also passed.

Remaining validation on the merged head:

- Run the range/anchor tests, including the new empty-demand direct/relay and
  sibling-isolation regression, then the full moq-net suite.
- Complete `just ci`, especially the selected Rust feature boundaries. The
  merged-head attempt passed its JavaScript checks and selected tests
  (including all 2,746 net tests), Markdown, GitHub harness, drill, shell,
  large-file, TOML, Nix, and Justfile checks. Rust remained at host admission;
  its waiting commands were stopped for a clean handoff.
- Run `just test interop --all`.
- Compile, smoke-test, and measure the new `session_range_updates` benchmark
  across both its group-span and concurrent-request axes.

Do not mark the PR ready until these checks finish. Keep the whole-group
standalone FETCH decision and the joining fill semantics above.

## Related

- [Pipelined first FETCH](/quest/m2/pipeline-requests/fetch.md) - sends TRACK_STATUS alongside the FETCH in adjacent code (`ietf/subscriber.rs`, `model/origin.rs`); an independent m2 optimization that adapts to these ranges and does not gate them
- [IETF fill timeout](/quest/m1/ietf-fill-timeout.md) - handles native timeout gaps; do not mistake them for never-existing objects or media-age limits
