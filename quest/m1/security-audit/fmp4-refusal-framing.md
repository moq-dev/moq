# [S] fMP4 refusals keep framing

## Goal

A box the fMP4 importer refuses at its header no longer leaves a caller
feeding arbitrary chunks out of frame. Either the importer reads the rest of
that decode call's input past the refused box, so the caller keeps framing
as a box-aligned caller does, or the refusal is terminal and every later
call fails, so no input is silently dropped mid-stream. The 4-track
interleaved import benchmark is no slower than before the streaming change,
or the cost is measured and accepted.

## Plan

Decided 2026-10-31 (maintainer, the /quest-complete requeue session), from
the follow-ups of [moq-dev/moq#7232](https://github.com/moq-dev/moq/pull/7232)
(fMP4 streams mdat) and [moq-dev/moq#7234](https://github.com/moq-dev/moq/pull/7234)
(MKV streams blocks). Both have landed. #7234 landed first, so #7232 already
streams the importer's samples through `Producer::open`/`commit`; the
original open/commit bullet is done, and this quest keeps the other two
([noted on moq-dev/moq#7468](https://github.com/moq-dev/moq/pull/7468#issuecomment-6554670247)).

- **A refused box.** `Import::decode` (`rs/moq-mux/src/container/fmp4/import.rs`)
  documents it: a refused box or fragment discards the rest of its body, and
  the rest of the refusing call's input is not read. That suits box-aligned
  callers (HLS ingest feeds one segment per call) but loses framing for
  arbitrary chunking. Either continue past the refused box, or make a
  refusal terminal for the importer as the MKV importer's is. Choose during
  implementation, by which keeps the importer's state simpler and what HLS
  ingest needs, and record the choice in the PR.
- **Benchmark.** #7232 measured a possible ~25% slowdown in the 4-track
  16-run whole-input `cmaf-interleaved-import` case, in a noisy run where the
  4-track cases swung ±40% between runs of one source. Re-measure it with the
  paired comparison on a quiet host, against the commit before #7232, and
  fix it or record why it is accepted.

Public API: none expected. Wire: none.

## Related

- [MKV leaf element limits](/quest/m1/security-audit/mkv-leaf-limits.md) - the other leftover of the importer streaming work
- [Media fuzz](/quest/m2/media-fuzz.md) - would catch a dropped-input bug like the refused-box one
