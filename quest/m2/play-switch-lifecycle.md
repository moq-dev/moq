# [M] moq play models each half's rendition lifecycle as one enum

## Goal

`moq play` (`rs/moq-cli/src/play/media.rs` `play_broadcast`, `rs/moq-cli/src/play/playback.rs` `Half`) tracks a half's rendition across several fields that must agree: `Half`'s `playing`, `switching`, `narrowed`, `track`, and `read`, plus the `video_opening`, `video_demand`, `video_released`, `video_shape`, and `replaced` locals in the select loop.
Each review round of [#9287](https://github.com/moq-dev/moq/pull/9287) found another combination of them that no phase allows, such as a turn's narrowed read cleared once its replacement started, or a replaced task's finish dropped.
Model each half's lifecycle as one enum whose variants carry their data, per the State section of `REVIEW.md`, so those combinations cannot be represented, with no change in what plays.

## Plan

- Recommended phases: idle (with the snapshot read and whether it was narrowed), opening (its candidates and future), playing (its demand, track, shape, and clock release), and replacing (the playing data plus the replacement's opening and whether it is a turn or a move).
- Keep the existing `playback` and `media` tests passing unchanged, and turn each `#9287` finding into a test of the transition it broke where the `Recorder` harness can drive it.
- Found by Codex on [#9287](https://github.com/moq-dev/moq/pull/9287), which kept the flags so it would not grow further.

## Related

- [play-pending-rendition](/quest/m2/play-pending-rendition.md) - adds another switch trigger to the same state, simpler once it is one enum
