# [M] moq-dev-js sees a namespace withdrawal from its own process

## Goal

The moq-dev-js runner client passes `subscribe-namespace-lifecycle` against
Cloudflare draft-18 and aiomoqt when its publisher and subscriber share one Bun
process, as it already does in two processes or paired with a Rust counterpart.

## Plan

Found while working
[moq-dev/moq#6275](https://github.com/moq-dev/moq/pull/6275): in one process
the subscriber never sees NAMESPACE_DONE. Each half passes with a Rust
counterpart, so suspect state shared between the two sessions in js/net or the
WebTransport polyfill. Reproduce with a js/net test first, fix at the cause, and
add the regression test there.

Public API: none expected. Wire: none.

## Related

- [Runner cases](/quest/m1/interop-coverage/runner-cases.md) - added the case to moq-dev-js
