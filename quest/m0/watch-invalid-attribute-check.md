# [XS] The invalid-attribute browser check matches moq-watch's attributes

## Goal

`test/interop/clients/js/publish-ui.browser.ts`'s "invalid attributes and
properties refuse, retain bad markup, and recover" passes on main again. Each
case tests an attribute moq-watch or moq-publish still has, and every value it
calls invalid is one the element documents as invalid.

## Plan

Planned 2026-11-07 (maintainer, /quest-complete) from
[moq-dev/moq#8136](https://github.com/moq-dev/moq/pull/8136)'s validation.
From the repository root, `(cd test/interop/clients/js && bun test ./publish-ui.browser.ts -t "invalid attributes")` fails on main
(9bf34b20dd) with `invalid: "30"` giving `threw: false, recovered: false`.

Likely cause: [moq-dev/moq#8057](https://github.com/moq-dev/moq/pull/8057)
renamed moq-watch's `buffer` to `lookahead`. In `js/watch/src/element.ts`, a
`buffer` attribute now throws only the rename error, and the element has no
`buffer` property. So the case `["moq-watch", "buffer", "30", "buffer", 0]`
neither throws on `Reflect.set` nor reads back its valid value. Confirm which
case fails. If it is this one, move it to `lookahead` with a value
`parseLookahead` refuses, and keep a check that `buffer` still reports its
rename when set and recovers once the attribute is removed.
If the `delay` case fails instead, decide from `parseDelay`'s documented
syntax whether the element or the test is wrong.
