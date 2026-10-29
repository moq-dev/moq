# [XS] The publish controls stay visible without a preview

## Goal

`<moq-publish>`'s controls stay visible when a camera is live but the element
shows no `<video>` or `<canvas>` preview.

## Plan

Reproduced on main by waiting for real camera capture in the existing
no-preview browser fixture: the player height becomes exactly zero. The
previous test raced capture startup, so its clicks could miss the collapse.

The implementation observes the slotted content's rendered size, retaining
the last rendered aspect ratio when empty (initially 16:9), rather than
inferring a preview from capture state. The demo's canvas consumer needs no
API change.

Remaining: validate the implementation in
`test/interop/clients/js/publish-ui.browser.ts`, including live capture without
a preview and video/canvas previews added, hidden, and removed. Run `just ci`
and exercise `just dev` in a real browser before completing the quest.
Host admission prevented these checks from running after the fix; formatting
and whitespace checks passed. The stale version-list browser expectations
have their own [fix PR](https://github.com/moq-dev/moq/pull/7060).

Decided 2026-10-28 (maintainer): m1.

Public API: none. Wire: none.
