# [XS] Kind::Named keeps its codec names

## Goal

`moq_audio::decode::Kind::Named` accepts what published moq-audio 0.1.6
accepts, the codec names (`"opus"`, `"aac"`, `"pcm"`), so the line can merge
to `main` without breaking callers. [#4131](https://github.com/moq-dev/moq/pull/4131)
switched it to backend names (`"libopus"`, `"symphonia"`, `"pcm"`) on the line
branch, which turns every existing `Named("opus")` or `Named("aac")` into
`Error::Unsupported` after an upgrade. This must land before the line PR
[#4081](https://github.com/moq-dev/moq/pull/4081) merges to `main`.

## Plan

- Decided: keep codec names; backends are picked internally. Which backend
  decodes AAC (AudioToolbox, Media Foundation, symphonia) is the library's
  choice through `Auto` and `Software`, not a string a caller has to know and
  that changes per platform. This reverses #4131's reasoning on purpose: the
  published contract wins over matching moq-video's backend naming.
- Make `encode::Kind` read the same way. It is unpublished, so drop `Named`
  there or give it the same codec meaning, whichever leaves the two enums
  mirrored; don't keep backend names on one side only.
- Tests that forced a backend by name move to a crate-private seam.
- Docs and the `Kind` doc comments list the codec names.

Public API: `decode::Kind::Named` returns to its published meaning, so the
line stays additive on `main`. Wire: none.
