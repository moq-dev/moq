# [S] Media element attributes reflect their state

## Goal

`<moq-publish>` and `<moq-watch>` reflect state changed through properties
or the built-in UI back to their attributes, so `[muted]` selectors,
attribute-diffing frameworks, and a later `setAttribute` behave. An invalid
attribute value is refused loudly, never silently replaced by a default:
the property setter throws, and an invalid value from markup surfaces as an
error the page can observe while the element does not run with a defaulted
configuration.

## Plan

[PR #6904](https://github.com/moq-dev/moq/pull/6904) implements state reflection,
strict URL/numeric/duration and source/rendering/catalog-mode refusal, invalid
markup suspension/recovery, and the five readonly watch fields. Reflection does
not replay setters, preserving selected `File` objects and existing disabled
connection state. Keep the PR draft for the remaining version-list decision.

The Goal requires invalid property setters to throw, with no exception for
`versions`. Unsupported version names and duplicate/empty lists currently fail
through `connection.error`. The shared `normalizeVersions` implementation is
`@internal` in `js/net/src/connection/connect.ts`; neither it nor the private
runtime registry is exported by `connection/index.ts`, the public package
entrypoint, or its `zod` subpath. No existing public validation primitive was
found. Creating disposable connections for validation or duplicating supported
version tables would add machinery instead of reusing the shared implementation.

The maintainer must choose:

1. Expose and reuse one shared version-list validation primitive for synchronous
   element refusal, preserving the Goal (recommended). Its public API placement
   and shape need selection before implementing it; test unsupported names,
   duplicate/empty property lists, and snapshotting with the existing defaults.
2. Explicitly exempt `versions` from synchronous element refusal and retain
   `connection.error` delegation, revising the Goal and migration description.

No choice has been made while the maintainer is away. Do not add a cross-package
API or relax the Goal on that basis. The 2026-10-25 strict-refusal decision remains
settled for the other attributes.

### Proof at checkpoint

The three initial Chromium regressions failed before fixing. The expanded real
browser cases cover property/UI/markup reflection, later repeated mute writes,
File identity, explicit false booleans, existing versions/default cases,
URL/volume and all source/rendering/catalog modes, invalid durations, observable
markup errors, multiple-invalid gating, and recovery. Existing mocked callback
cases moved into browser coverage, preserving their useful cases.

The normal `just dev` publish UI with fake devices and actual `bbb.hang` watch
surface passed, with screenshots and attribute/error snapshots retained in the
isolated checkout's `.tmp/`. Full harness passed, including 28 browser-side cases
and 14 compatibility cases. Browser TypeScript check passed.

Scratch-only integration with exact related PR heads passed: camera retention
[PR #6894](https://github.com/moq-dev/moq/pull/6894) at
`83eb3a7a6e99448ff6affc1b6f1c76b864e8e793`, and independent volume/mute
[PR #6895](https://github.com/moq-dev/moq/pull/6895) at
`811efb220585ea8f2c71a5947776d5b8400b9efa`. Both attribute/property orders keep
0.8 while muted, volume edits/unmute/zero reflect without feedback, and an
announced camera remains selected/announced after both tracks are disabled.
The first integration run lacked a preview canvas and a button lay outside the
viewport; its diagnostic is retained, and the corrected normal canvas layout
passes without changing production code, acceptance, or timeouts.

Related [PR #6751](https://github.com/moq-dev/moq/pull/6751) only changes allocator wiring in the elements and does not
settle these semantics. No related branch was merged or changed.
