# [S] Validate malformed container import refusal

## Goal

Complete the required clean-head validation of the MKV/FLV import fixes in
[moq-dev/moq#6908](https://github.com/moq-dev/moq/pull/6908), then remove this
quest. Malformed tracks, duplicate numbers and invalid timestamps fail;
unsupported roles alone may skip. Unsupported codecs fail in MKV and FLV.
VP9 dimensions preserve the complete 16-bit-plus-one range.

## Plan

The source and focused surface are implemented. No API or policy choice is
open. Keep the unrelated benchmark repair out of this PR.

- Run strict `just ci` on a compatible Nix builder. The clean-head attempt
  selects the flake gate and fails building the runner image because
  `unshare` cannot write `/proc/self/uid_map` on this host. Do not bypass
  the selector or avoid the direct dependency to hide the gate.
- The ancillary `just fix` all-target check also reaches an unrelated base
  error: `rs/moq-mux/benches/catalog.rs:226` refers to the removed
  `hang::catalog::MAX_RENDITIONS` (E0425). Untouched base `83d97bea0a`
  reproduces it with `cargo check --locked -p moq-mux --bench catalog`.
  Changed-source formatting is verified separately. Keep that repair a
  follow-up; it is not an unresolved importer implementation choice.
- Recheck integration with pending
  [moq-dev/moq#6750](https://github.com/moq-dev/moq/pull/6750),
  [moq-dev/moq#6751](https://github.com/moq-dev/moq/pull/6751),
  [moq-dev/moq#6752](https://github.com/moq-dev/moq/pull/6752) and
  [moq-dev/moq#6864](https://github.com/moq-dev/moq/pull/6864) when merging
  their base changes. The inspected patches have no competing importer
  invariant; 6864 independently edits fMP4 cases in the containers benchmark.

The initial six refusal reproductions all failed before production changes.
Seven-byte input also exposed the old replay offset bug: a buffered master
reported its payload offset, retaining orphan TrackEntry fields that bypassed
Tracks validation. The importer now owns the already-locked published
`ebml-iterable` 0.7.1 incremental decoder, with the same buffered master set.
Completed elements are consumed once, incomplete masters keep their children,
and EOF validates truncation before releasing the input allocation.

Checks cover constructor and chunked error propagation, malformed metadata,
duplicate/zero track numbers, unsupported roles/codecs, timestamp underflow
and overflow, truncated EOF, unknown-size live clusters, and VP9 dimensions
through 65536. A real generated live WebM fixture verifies all six emitted
Opus frames across one-byte and larger chunks; its provenance is beside it.
The existing finished-import allocation test remains unchanged. The
`mkv-chunks` containers benchmark exposes chunk-size parser regressions;
shared-host observations are descriptive, with no comparative speedup claim.

Public API shape and wire: unchanged. Accepted-input behavior tightens as
already decided on 2026-10-25: provide valid metadata and supported codecs,
or transcode/remove unsupported media tracks before import. Unsupported roles
remain skippable. No new error variant or timestamp range is introduced.
