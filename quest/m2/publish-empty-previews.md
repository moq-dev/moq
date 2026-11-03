# [XS] moq-publish hides previews that show nothing

## Goal

`<moq-publish>` hides a preview that renders nothing (an idle `<video>`
placeholder, a hidden camera's canvas), so its UI sizes purely from the
observed rendered size, with no capture gate.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31
/quest-complete run), from
[moq-dev/moq#7247](https://github.com/moq-dev/moq/pull/7247): m2, as
recommended. [moq-dev/moq#7247](https://github.com/moq-dev/moq/pull/7247) (merged) sizes controls from the
rendered preview and combines that with an `invisible` capture rule.

- In `js/publish/src/element.ts`'s preview effect (`:279` as of
  be39188000), set the empty previews to `display: none` while they have
  nothing to show. A `<video>` already is with no visible source (`:314`),
  but the `<canvas>` branch (`:285`) never hides, so an idle canvas keeps
  its 300x150 placeholder and a hidden camera's canvas keeps its size. Check
  the `<video>` idle state too, then drop
  [moq-dev/moq#7247](https://github.com/moq-dev/moq/pull/7247)'s capture gate from the size observer
  in `js/publish/src/ui/element.ts` (`:115`).
- Test in `test/interop/clients/js/publish-ui.browser.ts`, beside
  [moq-dev/moq#7247](https://github.com/moq-dev/moq/pull/7247)'s publish-controls cases, since sizing
  needs real layout: idle and hidden-camera states report an empty size; a
  live capture reports its rendered size.

Public API: none. Wire: none.
