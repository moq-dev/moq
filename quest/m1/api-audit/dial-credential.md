# [XS] One meaning for Credential

## Goal

`Credential` means only the public `auth::Credential`. The internal type
for what one dial presents, in `js/net/src/connection/tokens.ts` and moq-tokio's
`connect/token.rs`, gets its own name, and the `Presented` import alias in
`tokens.ts` goes away.

## Plan

[moq-dev/moq#6751](https://github.com/moq-dev/moq/pull/6751) renamed the
public `Auth` to `Credential`, which collides with these internal types.
Pick a name by role (such as `Presentation`) and use it in both languages.
Both types are internal, so nothing public changes.

Public API: none. Wire: none.
