# [XS] Drop the H.265 SPS pre-walk once scuffle is fixed

## Goal

moq-mux (and moq-video, once it shares the walk) parse H.265 SPS with
fixed scuffle-h265 and scuffle-expgolomb releases and no longer pre-walk the
SPS to refuse inputs that overflowed the old ones. The committed regression
inputs still pass.

## Waiting

- ScuffleCloud/scuffle: fixed releases for
  [ScuffleCloud/scuffle#653](https://github.com/ScuffleCloud/scuffle/issues/653)
  (scuffle-h265) and
  [ScuffleCloud/scuffle#654](https://github.com/ScuffleCloud/scuffle/issues/654)
  (scuffle-expgolomb) on crates.io. Check the issues
  and `curl -s https://crates.io/api/v1/crates/scuffle-h265` (and
  `scuffle-expgolomb`) for a version newer than 0.2.2 and 0.1.5. Posting
  on those repos still needs the maintainer's approval.

## Plan

Decided 2026-11-03 (maintainer, follow-up planning after the 2026-11-03 /quest-afk and /quest-complete runs), milestone:

- m0
- m1
- ✅ m2

[moq-dev/moq#7685](https://github.com/moq-dev/moq/pull/7685) added the walk
(`sps::parse_full`) because scuffle-h265 0.2.2 and scuffle-expgolomb 0.1.5
overflow on hostile SPS fields.

- Bump both crates, run the `fmp4`, `annexb`, and `ts` fuzz regressions and
  moq-video's SPS regression test against the bare parser, and delete the
  walk only if they pass without it.
- Keep every regression input.

Public API: removes the shared walk if moq-video no longer needs it.
Wire: none.
