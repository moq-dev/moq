# [S] Expired token error

## Goal

A session whose token expired reports `moq_net::Error::Expired`, not
`Unauthorized`, over moq-lite and moq-transport, mirrored in `@moq/net` and
the bindings. A client can then refresh its token instead of treating the
refusal as final.

## Plan

- Add the variant to the `#[non_exhaustive]` error, so the change is additive
  on `main`. Map it from lite's `AUTH_ERROR { Expired }` and moq-transport's
  `EXPIRED_AUTH_TOKEN`, and back again when refusing.
- Lite session code 0x30 is NOT_SUPPORTED
  ([Lite NOT_SUPPORTED](/quest/m1/auth/not-supported.md)). `Expired` takes a
  shared moq-transport session value if one means the same thing, otherwise
  0x31.
- Carry it through moq-ffi's error mapping and each wrapper.

Moved from m2 into the auth line: relay tokens refuse an expired
token with `AUTH_ERROR { Expired }`, so without this moq-net cannot tell it
apart from `Unauthorized`.

## Required

- [WebSocket refusal](/quest/m1/auth/ws-unauthorized.md) - both transports refuse a token at the session level
