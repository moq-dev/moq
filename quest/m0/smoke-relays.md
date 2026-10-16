# [M] Attribute public relay interop failures

## Goal

The failed optional cells in moq-dev/smoke's public relay matrix have
reproducible, attributed causes. First-party interoperability bugs have
focused fixes or their own quests; unavailable endpoints and unsupported
peer behavior remain explicit results rather than unexplained timeouts.

## Plan

All eight required `cdn.moq.dev` cells pass in the
[2026-10-15 relay job](https://github.com/moq-dev/smoke/actions/runs/38739639896/job/116437673069).
Optional failures affect imquic, Cloudflare draft-14 and draft-16,
moqtail draft-22 and draft-18, moqx, Moxygen, quiche-moq, and stitcher-moq.
These results do not make the workflow fail, but a green required-relay gate
does not prove those peers interoperable.

Use the per-cell logs and published report to distinguish connection or
endpoint availability from protocol, catalog, and media failures. Reproduce
against a local peer when possible; a public endpoint's state alone cannot
identify whose implementation is wrong. Preserve the publisher/subscriber
and transport distinctions: some peers fail only browser publishing or
particular subscribers.

Keep required versus optional policy unchanged. Do not remove endpoints,
skip failing supported versions, add exchange retries, or widen timeouts to
make the table green. The result of this diagnosis may be several
independently landable fixes or quests, rather than one cross-peer patch.
No external posts are authorized outside moq-dev and kixelated.

The maintainer requested fixes or quests for the investigated failures on
2026-10-15. The minimal Moxygen client failure has its own owner.

## Related

- [Moxygen minimal interop](/quest/m1/smoke-moxygen.md) - fixes the local announce-subscribe failure
- [Draft-22 release line](/quest/m0/release-22/README.md) - prior imquic release compatibility work
