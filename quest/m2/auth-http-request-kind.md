# [M] HTTP auth servers see which request named a token

## Goal

An external auth server authorizes per request: the HTTP auth contract
forwards `auth::Request`'s `kind()` and `path()` from
[moq-dev/moq#7211](https://github.com/moq-dev/moq/pull/7211), so the server can
decide a token's grant knowing which request (and path) first named it.

## Plan

Decided 2026-11-03 (maintainer, follow-up planning after the 2026-11-03 /quest-afk and /quest-complete runs), milestone:

- m0
- m1
- ✅ m2

#7211 made `moq_net::auth::RequestKind` public and gave `auth::Request`
`kind()` and `path()`, naming the request that first named the token
(`None` for an AUTH-stream token). moq-auth's HTTP client doesn't send them
yet.

- Add both to the HTTP auth request body as optional fields, absent for an
  AUTH-stream token, so existing servers keep working.
- Document the fields in the HTTP auth contract docs and `moq auth serve`,
  and have the reference server read them.
- Test that a server can refuse a token for one request kind and accept it
  for another.

Public API: additive (HTTP auth contract fields). Wire: none.

## Related

- [Token auth follow-ups](/quest/m2/token-auth-followups.md) - the rest of #7211's follow-ups
