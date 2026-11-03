# [S] Release's MKV importer bounds what it buffers

## Goal

On `release`, an MKV input that declares a huge element and trickles its
bytes no longer grows the importer's memory without limit: once the bytes
held for one incomplete element pass a size bound, the import fails with an
error. Ordinary files of any total size still import, and the published API
and supported inputs are unchanged.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31 /quest-complete run), from [moq-dev/moq#7234](https://github.com/moq-dev/moq/pull/7234).

Backport #7234's MKV unbounded-buffer fix to `release`? ✅ Trimmed
backport, m0: confirm with the 1 GiB repro on `release`, size bound only.

Why (the recommendation's reasoning, from #7234's Impact): #7234 is a real memory-exhaustion fix, but the full change is a
rewrite that also refuses inputs `release` accepts today (subtitle tracks,
CMAF output, video `BlockGroup`s with fields after their `Block`). A size
bound fixes the exhaustion on `release` without changing what it imports.

- **Confirm first**: port #7234's reproduction,
  `ignored_payload_does_not_accumulate_under_a_large_metadata_master` (a
  1 GiB EBML header holding a nearly 1 GiB Void, fed in 1 KiB chunks), onto
  `release` and show it grows memory there. If it doesn't, report that and
  stop.
- **Where it buffers**: `release`'s
  `rs/moq-mux/src/container/mkv/import.rs:139` builds ebml-iterable's
  `TagDecoder` to collect the `Ebml`, `Info`, `Tracks`, `TrackEntry`,
  `Audio`, `Video`, and `BlockGroup` masters whole, and `decode` (`:170`)
  keeps every byte of an incomplete tag in `self.buffer` until it completes;
  `SimpleBlock` and ignored binary leaves such as `Void` are held the same
  way.
- **The bound**: refuse once the bytes retained for the element in progress
  exceed a cap, checked in `decode` before appending more. `Segment` and
  `Cluster` arrive as start and end events and are never held whole, so a
  large file is unaffected. Do not set ebml-iterable's
  `max_allowable_tag_size`: it applies to every sized tag including
  `Segment`, which [moq-dev/moq#7234](https://github.com/moq-dev/moq/pull/7234)
  (merged) ruled out. Pick the cap from the largest legitimate block `release` can
  publish (its transport frame limit, if it has one), and name the choice in
  the PR. The cap applies to every held element, so it must also clear the
  largest legitimate `Tracks`/`TrackEntry` (CodecPrivate) and `Void`; if one
  cap can't cover both blocks and metadata, use a cap per element kind, or
  skip an ignored leaf such as `Void` without holding its bytes.
- Tests on `release`: the reproduction now fails with the error and
  bounded memory, a single element just over its cap fails while one just
  under imports, and an ordinary file larger than the cap still imports.
- Version bumps stay with a human, per `CONTRIBUTING.md`: this PR bumps
  nothing.

Public API: none; if refusing needs a new `mkv::Error` variant on
`release`, report it, since the enum is published. Wire: none.

## Related

- [Backport the relay and auth hardening to release](/quest/m0/backport-release-hardening.md) - other security backports for the same patch release
