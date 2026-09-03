# [M] Conferencing Demo

## Goal

Implement and verify the behavior tracked in [#697](https://github.com/moq-dev/moq/issues/697)
within the issue's stated scope and boundaries.

## Plan

Rescoped during the 2026-08 grooming: the hang-meet component this issue
builds on no longer exists. Compose the demo from the current js/publish,
js/watch, and demo/web primitives instead.

### Issue context

There is a [hang-meet](https://github.com/kixelated/moq/tree/main/js/hang/src/meet) web component but it's extremely simple.

We should steal the best, generic stuff from `hang.live` to create a proper conferencing demo. I could set up the rendering but I would want some help on the rest, especially the UI. Let me know if you're interested and I can set up the base.

## Closes

- [#697](https://github.com/moq-dev/moq/issues/697) - close this issue when the quest finishes
