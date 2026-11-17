# [S] JS learns the verdict on its connection credential

## Goal

A reconnecting JS `Connection` tells the app the peer's verdict on the
tokens it last set, as Rust's `Tokens::poll_answered` does: granted, or the
peer's refusal, a close that refuses the connection credential included. An
app that re-signs its credential for each dial, such as kixel.ai's `Device`,
can then end the connection when a device that was live is removed or
revoked. Non-goals: changing when the loop retries
([moq-dev/moq#8618](https://github.com/moq-dev/moq/pull/8618) keeps
retrying a refusal after a healthy session, and
[moq-dev/moq#8817](https://github.com/moq-dev/moq/pull/8817) ends only a first
session refused before it is healthy), and in-band AUTH retry hints
([moq-dev/moq#8836](https://github.com/moq-dev/moq/pull/8836)).

## Plan

Decided 2026-11-15 (maintainer, `/quest-plan` from kixel.ai's `/quest-run`,
for kixel.ai's `quest/m0/mesh/refused.md`): ✅ JS parity with Rust's
`Tokens` verdict (rejected: no moq quest, relying on first-dial-refusal
alone, which leaves a removed live device redialing forever; changing the
retry policy, which reverses #8618).

JS already replaces the tokens (`Reload.tokens`, a `Signal`, presented at
once on a live session with AUTH and by every later dial), but exposes no
verdict: `Connection` hides its sessions, and a refused redial leaves the
grant undefined, so the app cannot tell a refusal from a drop.

Recommended shape, settled with its consumer: the reconnecting connection
exposes the verdict on the latest `tokens` set, mirroring
`Tokens::poll_answered` (`rs/moq-net/src/auth/tokens.rs`) rule for rule. A
token the set kept, the connection credential included, counts only once
the peer refuses it, and nothing is re-checked after the answer. The session
the latest dial built counts its close as the answer when that close refuses
the connection credential, on versions without AUTH too. The app ends the
loop by aborting its auth handle, which already makes the loop give up with
that error.

Tests, beside `js/net/src/connection/tokens.test.ts` and `reload.test.ts`: a
set granted in band on lite-07, a set refused by a closing session on a
version without AUTH, a kept credential that counts only on refusal, and a
set replaced before its answer.

Public API: an addition to `@moq/net`'s `Connection`. Wire: none.
