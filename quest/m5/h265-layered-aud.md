# [S] An MV-HEVC stream with access unit delimiters stays one frame per access unit

## Goal

moq-mux imports an x265 MV-HEVC stream encoded with `--aud` as one frame per
access unit, as it already does without `--aud`, or refuses it with an error
that names the cause, rather than splitting each view into its own frame.

## Plan

Planned 2026-11-08 (maintainer, approved follow-up of
[moq-dev/moq#8199](https://github.com/moq-dev/moq/pull/8199)).
Moved to m5 2026-11-12 (maintainer, /quest-audit): a feature with no named
consumer.

x265 4.2 with `--num-views 2 --aud` writes a layer-0 AUD between the base
and enhancement views. `rs/moq-mux/src/codec/h265/split.rs` closes the open
access unit on any `NalType::AUD`, which H.265 F.7.4.2.4.4 requires for a
layer-0 AUD, so each view becomes its own frame.

- First find out whether that stream conforms. If x265 is wrong, report it
  (with the [upstream reports](/quest/m4/upstream-reports.md)) and make the
  splitter refuse that specific violation with an error naming it, instead
  of adding a heuristic. Annex F allows an access unit with no layer-0
  picture, so the refusal never rejects those by itself.
- If it conforms, frame by what the spec says starts an access unit and add
  the x265 `--aud` fixture beside #8199's.

Public API: none. Wire: none.
