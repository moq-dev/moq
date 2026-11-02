# [S] moq-c closes its small API gaps

## Goal

moq-c stamps a JSON window entry with a caller's timestamp and can disable a
simulcast rung on pre-encoded media.

## Plan

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

Public API: moq-c gains an optional caller timestamp on JSON window push and
pop, and a call to disable a rung of pre-encoded media. Wire: none.
