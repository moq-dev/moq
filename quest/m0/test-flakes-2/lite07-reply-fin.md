# [S] A lite-07 reply FINs instead of resetting when the subscriber FINs mid-write

## Goal

When a lite-07 subscriber FINs its subscription stream while the publisher's
reply write is yielding on the shared write budget, the publisher still FINs
in reply instead of resetting with Cancel. `js/net/src/lite/publisher.test.ts`'s
"lite-07 answers a bounded window that resolves empty and FINs in reply"
passes under `--rerun-each 200`, and its `test.skip` is removed.

## Plan

Planned 2026-11-07 (maintainer, /quest-complete) from
[moq-dev/moq#7670](https://github.com/moq-dev/moq/pull/7670)'s validation:
the test fails on main 12 times in 200 under `bun test --rerun-each 200`. It
is quarantined with `test.skip` until this quest fixes it, per AGENTS.md.

The bug is in the product, not the test. `Writer.write`
(`js/net/src/stream.ts`) yields on the shared write budget after its bytes
are already queued. During that yield, `SubscriptionControls.response`
(`js/net/src/lite/publisher.ts`) takes the subscriber's clean FIN as
overriding the blocked write, so the publisher resets the stream.
[moq-dev/moq#8047](https://github.com/moq-dev/moq/pull/8047) fixed the same
yield's race for opened group streams only.

Candidate fixes:

- a clean FIN no longer overrides a reply write, and only an error does;
- `Writer.write` yields before handing its bytes over, not after.

Prefer the one that also covers every other `Writer.write` caller whose peer
can FIN during the yield. Check whether the Rust publisher has the same
ordering. Prove the fix with a test that forces the yield, not with repeated
runs.
