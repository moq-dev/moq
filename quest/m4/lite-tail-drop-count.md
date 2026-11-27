# [M] moq-lite session tails count drops instead of waiting a grace

## Goal

From lite-05 on, a moq-lite session's tail is complete once every group up to the end has arrived or been reported in SUBSCRIBE_SKIP or SUBSCRIBE_DROP. The grace (`tail::grace`, the subscription's max delay or else the 1 s `ledger::GRACE` in `rs/moq-net/src/ledger.rs`) then no longer decides it, in Rust and JS.

## Plan

Decided 2026-10-30 (maintainer, follow-up planning). Found by the audit on [moq-dev/moq#7403](https://github.com/moq-dev/moq/pull/7403). The maintainer dislikes grace periods.

Checked 2026-11-12 at a86433043: the counting half already exists. `Tail::account` (`rs/moq-net/src/tail.rs`) records groups a SUBSCRIBE_SKIP or SUBSCRIBE_DROP accounts for, and `lite/subscriber.rs` calls it for skips, drops, and served fill ranges, so a tail whose every group is accounted for completes without waiting. The grace still decides only the gaps nothing accounts for: a group stream reset before its header arrived (the `tail.rs` module docs) and, on lite-05/06, a lost datagram. That remainder is this quest.

- Count per version. lite-01..04 have no drop message, so they keep today's behaviour.
- On lite-05/06, account for holes left by lost datagrams. Lite-07 stream tracks carry no datagrams, which have their own track kind.
- Account for a stream reset before its header, so no gap is left only to the grace. Check whether the publisher already reports such a group in a SUBSCRIBE_DROP; if it needs a new signal, that is a lite-07-wip draft change, and the Wire line below changes with it.
- Guard against a peer that never accounts for a group. Note (2026-11-26): on lite-07, [moq-dev/moq#9686](https://github.com/moq-dev/moq/pull/9686) (`Tail::proven()`) and [moq-dev/moq#9565](https://github.com/moq-dev/moq/pull/9565) (`RESET_STREAM_AT`) already eliminated grace timers and `SUBSCRIBE_SKIP` (`0x3`), superseding the lite-07 half of the 2026-11-15 decision below; only the legacy lite-05/06 remainder (lost datagrams, pre-header resets, JS parity) stays here. Decided 2026-11-15 (maintainer, /quest-complete): ✅ (c) wait out the subscription's max delay where one is set, and ✅ (a) end a lite-05/06 tail with no max delay with an error (rejected: keeping the 1 s constant there). Don't add a new grace.
- The IETF tail keeps its grace until the moq-transport gap-signal extension (planned in [moq-dev/moq#7425](https://github.com/moq-dev/moq/pull/7425)) lands, since moq-transport can't signal gaps.

Public API: none. Wire: none.
