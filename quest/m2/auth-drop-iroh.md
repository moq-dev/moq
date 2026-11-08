# [XS] Drop moq-auth's iroh transport

## Goal

The next breaking moq-auth release drops `Transport::Iroh`
(`rs/moq-auth/src/request.rs`) and the `"iroh"` transport value in
`js/auth/src/contract.ts`, which nothing sends once the iroh backend is gone.

## Plan

Decided 2026-10-31 (maintainer, the /quest-complete requeue session), a
follow-up of [moq-dev/moq#7345](https://github.com/moq-dev/moq/pull/7345).
That PR kept both because the enum is the relay-to-auth-server JSON contract
and its quest said "Wire: none"; the relay just never sends `iroh` now.

Starts after #7345 lands; check with `gh pr view 7345`. Land it with the
first moq-auth release that already breaks for another reason, rather than
cutting a breaking release for this alone. The relay sends the transport
name and the auth server reads it. An old auth server is unaffected, since
a new relay never sends `iroh`. A new auth server still accepts `iroh` from
an older relay: an unrecognized name deserializes to `Unknown`
(`#[serde(other)]` in Rust, `z.catch` in JS). Update the auth contract docs
and any test or fixture naming `iroh`.

Moved from m4 to m2: Decided 2026-11-07 (maintainer, /quest-plan from the API sweep of open PRs and remaining quests). This cut already breaks moq-auth (for example [moq-dev/moq#8088](https://github.com/moq-dev/moq/pull/8088) removes the peer grants), so it is that release.

Public API: breaking in moq-auth and `@moq/auth` (one enum variant). Wire:
the auth JSON contract loses the `iroh` transport value.

## Related

- [The relay refuses a cluster peer it cannot dial](/quest/m3/relay-undialable-peers.md) - the other iroh leftover
