# [S] moq-c closes its small API gaps

## Goal

moq-c reports a malformed epoch the same way everywhere, lets a C consumer see
that audio is disabled as it can for video, has a test of the hops on a route
received through a relay, stamps a JSON window entry with a caller's timestamp,
and can disable a simulcast rung on pre-encoded media.

## Plan

Found 2026-10-28 by [moq-dev/moq#7151](https://github.com/moq-dev/moq/pull/7151)
and [moq-dev/moq#7156](https://github.com/moq-dev/moq/pull/7156).

Decided 2026-10-28 (maintainer): one PR for all of these. moq-c already breaks
at this cut (0.6 to 0.7), so the error-code change rides that break.

- `moq_broadcast_request_prepare` reports a malformed epoch as
  `MOQ_ERROR_INVALID_CODE`, while `moq_publication_announce`, which takes an
  epoch in `moq_announce_route` since
  [moq-dev/moq#7151](https://github.com/moq-dev/moq/pull/7151), reports a
  malformed one as `MOQ_ERROR_INVALID_CONFIG`. Use
  `MOQ_ERROR_INVALID_CONFIG` for both, with an Impact migration note.
- Add an audio counterpart to `moq_consume_video_enabled`, so a C app can tell
  that a held audio rendition is disabled.
- Add a moq-c test that announces through a relay session and checks the
  received route's `hops`.

- `moq_publish_json_window_push` and `_pop` always stamp the broadcast clock.
  Let them take an optional timestamp like the JSON snapshot and stream
  writers do since [moq-dev/moq#6946](https://github.com/moq-dev/moq/pull/6946).
  Decided 2026-10-30 (maintainer, /quest-plan of the 2026-10-30 /quest-complete follow-ups):
  ✅ add it here / drop.
- Add C ABI support for disabling simulcast rungs on pre-encoded media, which
  [moq-dev/moq#7029](https://github.com/moq-dev/moq/pull/7029) added to moq-ffi
  for OBS. Decided 2026-10-30 (maintainer, /quest-plan of the 2026-10-30 /quest-complete follow-ups): the C epic's rule (every m1
  binding feature missing from `moq.h` lands before the cut) overrides
  #7029's decision 4, which parked it until a C consumer asked:
  ✅ add to m1 C gaps / exempt it.

Update `moq.h`, `rs/moq-c/README.md` and `doc/lib/c` as needed.

Public API: moq-c gains an audio enabled query and changes one error code.
Wire: none.

