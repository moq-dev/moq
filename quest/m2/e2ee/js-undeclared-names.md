# [M] Encrypted JS broadcasts serve undeclared track names

## Goal

`@moq/net` exposes a public broadcast request handler, and `@moq/e2ee`'s
broadcast producer answers requests for track names it never created
through it, under semantic names, as
[undeclared names](/quest/m2/e2ee/undeclared-names.md) does in Rust. A JS
application then serves on-demand tracks from a plaintext or encrypted
broadcast the same way.

## Plan

Decided with the maintainer on 2026-10-24:

- One quest, both halves. Today the JS broadcast's request pull
  (`requested()` in `js/net/src/broadcast.ts`) is reachable only by wire
  layers, and only the origin has a public `dynamic()`. Expose the
  broadcast's handler under the name and shape that
  [named requests](/quest/m1/api-audit/requests.md) gives Rust's
  (`requests()` returning `Requests`), and keep the internal wire path on it.
- `@moq/e2ee` mirrors the Rust handler exactly: it opens each request's
  physical name, claims it, and yields a request whose `name` is semantic.
  A name that does not open is answered as missing and keeps no state. A
  re-requested name follows the rule undeclared-names settles.
- The handler offers the lister, taking semantic names and listing their
  physical names, as undeclared-names does.
- Port undeclared-names' tests: a relay-forged name, an older epoch's name,
  and a burst of forged names that leaves no state behind.

Public API: a public broadcast `requests()` in `@moq/net`, and a request
handler on `@moq/e2ee`'s broadcast producer. Wire: none.

## Required

- [Named requests](/quest/m1/api-audit/requests.md) - the handler name and shape this exposes
- [Undeclared track names](/quest/m2/e2ee/undeclared-names.md) - the Rust handler this mirrors
- [JS mirror](/quest/m2/net-wrap/js.md) - the `@moq/net` interfaces the `@moq/e2ee` wrappers implement
