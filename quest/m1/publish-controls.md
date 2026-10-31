# [XS] The publish controls stay visible without a preview

## Goal

`<moq-publish>`'s controls stay visible when a camera is live but the element
shows no `<video>` or `<canvas>` preview.

## Plan

Found 2026-10-28 by [moq-dev/moq#7157](https://github.com/moq-dev/moq/pull/7157),
and present on main: the controls box is sized by whether a camera source
exists, not by whether a preview is showing, so it collapses to zero height
and the controls leave the screen. #7157's browser test added a `<video>`
preview to its page to work around it.

Decided 2026-10-28 (maintainer): m1.

Size the box by what it shows, and cover it in
`test/interop/clients/js/publish-ui.browser.ts` with a page that has no
preview.

Public API: none. Wire: none.
