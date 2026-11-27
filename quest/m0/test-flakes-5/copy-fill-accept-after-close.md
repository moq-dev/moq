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
`write(defined(fill.accept(key)), key, "old")`, which runs detached from the
test. It throws whenever `fill.accept(key)` returns `undefined` while `closed`
is still `false`. The cause is not established yet; check:

- when `accept` returns `undefined`;
- whether any case ends without reaching `close()`;
- whether the detached loop can still call `accept` after `close()`.

If `accept` can return `undefined` while a case is still running, the fix
belongs in the fill path, not in `close()`. Never ignore a missing group.

Kept (maintainer, 2026-11-27 merge pick of
[moq-dev/moq#9735](https://github.com/moq-dev/moq/pull/9735)).
