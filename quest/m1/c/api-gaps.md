# [S] moq-c disables pre-encoded simulcast rungs

## Goal

moq-c can disable a simulcast rung on pre-encoded media, with its lower
rendition's priority and a rate control driven by the reservation.

## Plan

- Add C ABI support for disabling simulcast rungs on pre-encoded media on
  [moq-dev/moq#7029](https://github.com/moq-dev/moq/pull/7029)'s shared
  importer support. It merged as
  [61cb70456d](https://github.com/moq-dev/moq/commit/61cb70456dd0d70909bc2a1c18916331e90b51ea),
  so this work is unblocked: main has `moq_mux::import::Track::set_enabled`,
  `import::VideoInit.priority`, the AV1/VP8/VP9 importer setters, and the
  moq-ffi `MoqRateControl`, `MoqVideoInit.lower`, and media producer
  `set_enabled` interfaces.
- Expose media `set_enabled`, lower-rendition ordering, and
  a rate control over the C reservation. Inspect the final shared interfaces
  first. Decided 2026-10-30 (maintainer, /quest-plan of the 2026-10-30
  /quest-complete follow-ups): the C epic's rule (every m1 binding feature
  missing from `moq.h` lands before the cut) overrides that PR's decision 4,
  which parked it until a C consumer asked: ✅ add to m1 C gaps / exempt it.

Update the generated `moq.h`, `rs/moq-c/README.md` and `doc/lib/c` as needed.
The independent JSON window caller timestamp gap is implemented in
[moq-dev/moq#7614](https://github.com/moq-dev/moq/pull/7614).

Public API: moq-c gains pre-encoded rung disable, lower-rendition ordering,
and reservation rate control. Wire: none.

## Required

- [OBS rung disable](/quest/m1/simulcast-rung-disable.md) - land the shared importer and FFI prerequisites in [moq-dev/moq#7029](https://github.com/moq-dev/moq/pull/7029) before adding their C ABI counterparts
