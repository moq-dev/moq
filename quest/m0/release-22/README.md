# [XS] Tell draft-22 testers the release fixes shipped

## Goal

The draft-22 interop testers and the reporters of #5407 and #5413 know which
published releases contain their fixes. Delete this quest after the notices
are sent.

## Plan

Verified 2026-10-19:

- Draft-22 LOCATION_FILTER, FIRST_OBJECT at object 0, and moq-noq 1.3.5
  shipped in moq-relay 0.17.3 and `@moq/net` 0.4.3.
- moq-relay 0.17.5 shipped draft-20+ FETCH (#5402), the unset-clock fix
  (#5448, closing #5407), the quiet-track rejoin fix (#5430, closing #5413),
  and the tolerant catalog parser (#5520). All are in the current 0.17.7.
- npm publishes `@moq/publish` 0.5.3 and `@moq/hang` 0.5.3 from release
  commit `0eff2f63ce7bea8451bec698ba01cdebc5f73120`. They contain the
  unset-clock and tolerant-parser changes. `@moq/net` is now 0.4.4.
- `just test wire-compat --remeasure` passed against moq-cli 0.14.7,
  moq-relay 0.17.7, hang 0.21.16, and `@moq/hang` 0.5.3. It removed the
  `catalog-archive` exception and refreshed the existing unpublished
  lite-07-wip exception; stable lite-01 through lite-06 cells passed.

Remaining work: tell the draft-22 testers that the fixes are published,
and post release notices on #5407 and #5413. No further release cut or
version bump is needed. Reporter notices have not been sent.
The testers' rerun is welcome but does not gate completion.
