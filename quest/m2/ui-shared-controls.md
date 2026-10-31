# [M] Shared fullscreen, formatters, and stats cards in @moq/ui

## Goal

`@moq/watch` and `@moq/publish` import their fullscreen controller,
fullscreen button, human-readable formatters, and stats card and row builders
from `@moq/ui` instead of each keeping a copy. Each panel keeps its own
metrics and subscriptions. Additive exports only; no wire impact.

## Plan

Decided 2026-10-27 (maintainer interview, from the 2026-10-27 refactor audit):
include the stats primitives, not just fullscreen and formatters.

- The two `ui/fullscreen.ts` copies differ only in comments and where
  `enterPseudo`/`exitPseudo` are declared. The two `components/fullscreen-button.ts`
  and `ui/format.ts` copies are near-identical; watch's `format.ts` adds
  `formatMillis`.
- `card` and `line` in `js/watch/src/ui/stats.ts` and
  `js/publish/src/ui/components/stats-tab.ts` are byte-identical. Move those
  and the matching stats styles; leave `rate`, `track`, `trackActive`, and the
  metric wiring in each package.
- The shared controller exposes its state as a signal instead of today's
  `subscribe` listener set, per the repo's no-callback rule.
- Verify in a browser: real fullscreen, the iPhone pseudo-fullscreen class,
  a rejected request (not a user gesture), and teardown removing the document
  listeners.
