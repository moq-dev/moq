# [S] fMP4 import streams samples through open and commit

## Goal

The fMP4 importer streams its Legacy samples through the container
`Producer::open`/`commit` API instead of its own
`start_sample`/`finish_sample` pair, a box refused at its header no longer
drops the rest of the input handed to that decode call, and the 4-track
interleaved import benchmark is no slower than before the streaming change,
or the cost is measured and accepted.

## Plan

Decided 2026-10-31 (maintainer, the /quest-complete requeue session), from
the follow-ups of [moq-dev/moq#7232](https://github.com/moq-dev/moq/pull/7232)
(fMP4 streams mdat) and [moq-dev/moq#7234](https://github.com/moq-dev/moq/pull/7234)
(MKV streams blocks).

Starts after both land. They are queued PRs that finish and delete the
`fmp4-stream-mdat` and `mkv-stream-blocks` quests, so they are named here
rather than linked under Required. Check with `gh pr view 7232` and
`gh pr view 7234`.

- **open/commit.** #7234 adds `Producer::open`/`commit` for the MKV
  importer's streamed blocks. Move the fMP4 importer's streamed Legacy
  samples (`start_sample`/`finish_sample` in
  `rs/moq-mux/src/container/fmp4/import.rs` after #7232) onto it, so both
  importers stream a frame one way, and delete the fMP4-only pair.
- **A refused box.** After #7232, a box refused at its header ends the decode
  call and drops whatever input followed it in that call. Either continue
  past the refused box, or make a refusal terminal for the importer as the
  MKV importer's is. Choose during implementation, by which keeps the
  importer's state simpler and what HLS ingest (which feeds one segment per
  call) needs, and record the choice in the PR.
- **Benchmark.** #7232 measured a possible ~25% slowdown in the 4-track
  whole-input interleaved import benchmark, in a noisy run. Re-measure it
  with the paired comparison on a quiet host, before and after this change,
  and fix it or record why it is accepted.

Public API: none expected. Wire: none.

## Related

- [MKV leaf element limits](/quest/m1/security-audit/mkv-leaf-limits.md) - the other leftover of the importer streaming work
- [Media fuzz](/quest/m2/media-fuzz.md) - would catch a dropped-input bug like the refused-box one
