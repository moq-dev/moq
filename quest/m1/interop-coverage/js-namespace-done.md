# [S] JS namespace lifecycle passes with both halves in one process

## Goal

The runner's moq-dev-js `subscribe-namespace-lifecycle` case passes against
Cloudflare draft-18 and aiomoqt, where today the subscriber never sees
NAMESPACE_DONE. If the cause is in the relay under test, it is reported
upstream and the cell is classified instead.

## Plan

Found in the runs for
[englishm/moq-interop-runner#173](https://github.com/englishm/moq-interop-runner/pull/173)
on 2026-10-22. It fails only when the JS publisher and subscriber share one
process. Each half passes with a Rust counterpart, and two separate Bun
processes pass, so suspect state the two `js/net` sessions share in one
process before suspecting the relays. A fix in `js/net` gets a test there.

Public API: unknown until the cause is found. Wire: none expected.
