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

Land it with the first moq-auth release that already breaks for another
reason, rather than cutting a breaking release for this alone. An auth
server built against the old contract is unaffected, since an unknown name
deserializes to `Unknown`. Update the auth contract docs and any test or
fixture naming `iroh`.

Public API: breaking in moq-auth and `@moq/auth` (one enum variant). Wire:
the auth JSON contract loses the `iroh` transport value.

## Related

- [The relay refuses a cluster peer it cannot dial](/quest/m2/relay-undialable-peers.md) - the other iroh leftover
