# [S] The copy fill harness accepts no group after its test ends

## Goal

`js/net/src/lite/copy_fill.test.ts` reports no unhandled error between tests,
alone and in a loaded `just check`, fixed at its cause.

## Plan

Seen on 2026-11-26 in a local `just check` of `origin/main` `9ad325dbce`
with an unrelated fix applied, then reproduced on `9ad325dbce` itself:
`bun test src/lite/copy_fill.test.ts --rerun-each 20` (in `js/net`) passed
all 560 cases and still reported `1 error`, the same one each time:

> `# Unhandled error between tests`: `error: expected a value` at
> `defined` (`defined.test-util.ts:3`), from `copy_fill.test.ts:66`

That line is the publisher's fill loop in `peers()`:
`write(defined(fill.accept(key)), key, "old")`. That loop runs detached from
the test, so a fill still open when the test ends keeps asking for keys.
`fill.accept` then returns `undefined` before `close()` sets `closed`, and
`defined` throws outside any test. Find what leaves the fill open past the
test's end, and end the loop with the fill or wait for it in `close()`,
rather than ignoring a missing group.

Kept (maintainer, 2026-11-27 merge pick of
[moq-dev/moq#9735](https://github.com/moq-dev/moq/pull/9735)).
