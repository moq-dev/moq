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
- Carry it through moq-ffi's error mapping and each wrapper.

## Required

- [In-band auth](/quest/m1/auth/README.md) - the AUTH streams that carry these codes
