# [S] moq-video shares moq-mux's bounded H.265 SPS walk

## Goal

A hostile publisher's H.265 SPS can't panic a subscriber's decoder.
moq-video's VUI reads (`rs/moq-video/src/decode/vui.rs:102` and `:263`) stop
calling `scuffle_h265::SpsNALUnit::parse` directly and go through moq-mux's
bounded walk, `sps::parse_full` from
[moq-dev/moq#7685](https://github.com/moq-dev/moq/pull/7685), which refuses an
SPS that would reach scuffle-h265's overflowing lines. The regression inputs
from #7685 pass through moq-video's decode path too.

## Plan

Decided 2026-11-03 (maintainer, follow-up planning after the 2026-11-03 /quest-afk and /quest-complete runs), milestone:

- ✅ m0 (security: a remote input crashes a subscriber)
- m1
- m2

From #7685's follow-ups: it bounds moq-mux's import path, but moq-video still
parses the same SPS unchecked, and `:263` unwraps the result.

- Expose the walk from moq-mux (`codec/h265/sps.rs`) as a small public
  function rather than copying it into moq-video, so both crates refuse the
  same inputs.
- Replace both call sites; `:263`'s `unwrap()` becomes an error the decoder
  reports.
- Feed #7685's minimized inputs through moq-video's VUI read in a test that
  panics without the fix.

Public API: additive in moq-mux (the shared SPS walk). Wire: none.

## Required

- [SPS fields cannot overflow](/quest/m1/fuzz-findings/sps-overflow.md) - lands `sps::parse_full` in moq-mux (moq-dev/moq#7685)

## Related

- [Drop the H.265 SPS pre-walk](/quest/m2/scuffle-sps-bump.md) - removes this walk once scuffle ships fixed releases
