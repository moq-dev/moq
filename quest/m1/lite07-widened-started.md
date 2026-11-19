# [S] The @moq/net widened-range SUBSCRIBE_STARTED test passes again

## Goal

`js/net/src/lite/subscriber.test.ts` "a widened range's SUBSCRIBE_STARTED
resolves the feed's start" passes on `main`, with whatever it found fixed
at its root: after an update widens a lite-07 subscription's ranges, the
copy's start (`hooks.sourceStart`) stays pending until the widened range's
SUBSCRIBE_STARTED says where it starts.

## Plan

Found by [moq-dev/moq#9132](https://github.com/moq-dev/moq/pull/9132)'s
`just check`. It fails deterministically, with `sourceStart` reading `0n`
right after the update, where the test expects `"pending"`:

```sh
cd js/net && bun test src/lite/subscriber.test.ts -t "a widened range's SUBSCRIBE_STARTED"
```

It passes on 928d337396's parent and fails from
[moq-dev/moq#9045](https://github.com/moq-dev/moq/pull/9045) on, which
changed how a lite-07 relay fits its ranges into one SUBSCRIBE_UPDATE. The
merge queue runs only the tests for changed files, so it landed unseen.

- Decide whether the update's new range still needs its own
  SUBSCRIBE_STARTED under #9045's fitting. If it does, fix `#declare` or
  the update path; if a covering superset now legitimately keeps the start,
  rewrite the test to that rule, mirrored by the Rust test that covers it.
