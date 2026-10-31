# [XS] moq-publish hides previews that show nothing

## Goal

`<moq-publish>` hides a preview that renders nothing (an idle `<video>`
placeholder, a hidden camera's canvas), so its UI sizes purely from the
observed rendered size, with no capture gate.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31
/quest-complete run), from
[moq-dev/moq#7247](https://github.com/moq-dev/moq/pull/7247): m2, as
recommended. Start after [moq-dev/moq#7247](https://github.com/moq-dev/moq/pull/7247) (open) lands; it sizes controls from the
rendered preview and combines that with an `invisible` capture rule.

- In `js/publish/src/element.ts` / `preview.ts`, set the empty previews to
  `hidden` (or `display: none`) while they have nothing to show, then drop
  [moq-dev/moq#7247](https://github.com/moq-dev/moq/pull/7247)'s capture gate from the size observer.
- Test in `js/publish/src/element.test.ts`: idle and hidden-camera states
  report an empty size; a live capture reports its rendered size.

Public API: none. Wire: none.
