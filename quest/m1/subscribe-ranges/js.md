# [M] JS ranges

## Goal

Finish the explicit-end cleanup in `@moq/net`. The range request surface and
fills landed in [moq-dev/moq#7696](https://github.com/moq-dev/moq/pull/7696).

## Plan

Lite-07 bounded completion follows
[Lite-07 ranges](/quest/m1/subscribe-ranges/lite.md)' explicit end, which
[moq-dev/moq#7675](https://github.com/moq-dev/moq/pull/7675) brought to JS
alongside Rust: neither SUBSCRIBE_END nor a covered bounded subscription closes
the Subscribe Stream. The JS publisher keeps reading updates and answers a held
start with SUBSCRIBE_START and its held drops once no group can resolve it, and
the JS subscriber FINs once its tail settles past SUBSCRIBE_END, judging it only
after SUBSCRIBE_START.

What remains:

- **The explicit end's leftovers.** Rename SUBSCRIBE_END to SUBSCRIBE_FINISH in
  `js/net`, as Rust and the draft did, and delete the subscriber's covered-FIN
  path (`#parkCovered`, `#demandCovered`, the `short` reopen), which no lite-07
  publisher triggers any more.

## Related

- [Lite-07 ranges](/quest/m1/subscribe-ranges/lite.md) - the Rust lite-07 range fill this mirrors
- [Lite-07 FETCH is single-group only](/quest/m1/subscribe-ranges/lite07-fetch.md) - follows this and Lite-07 ranges
