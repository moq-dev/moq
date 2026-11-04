# [M] Encrypted JS broadcasts serve undeclared track names

## Goal

`@moq/crypto`'s broadcast producer answers requests for track names it never
created, under semantic names, through `@moq/net`'s public broadcast request
handler, as `moq_crypto::e2ee::broadcast::Requests` does in Rust. A JS
application then serves on-demand tracks from a plaintext or encrypted
broadcast the same way.

## Plan

Decided with the maintainer on 2026-10-24:

- One quest, both halves; amended 2026-11-03 (maintainer): the `@moq/net`
  half moved to [its own quest](/quest/m2/net-broadcast-requests.md), which
  plaintext users (Kixel's T3 host) need first, and this keeps the
  `@moq/crypto` half.
- `@moq/crypto` mirrors the Rust handler exactly: it opens each request's
  physical name, claims it, and yields a request whose `name` is semantic.
  A name that does not open is answered as missing and keeps no state. A
  re-requested or re-created name resumes above its high-water marks, as in
  Rust (decided 2026-10-25, maintainer: resume always, replacing the
  2026-10-17 refusal, so every identity stays unique and a handler can serve
  a dropped name again).
- The handler offers the lister, taking semantic names and listing their
  physical names, as Rust's `moq_crypto::e2ee::broadcast::Lister` does, over
  the broadcast lister `@moq/net` exports with its handler.
- Port Rust's `requests_refuse_names_the_generation_did_not_issue` and
  `requests_serve_a_dropped_name_above_its_marks` tests: a relay-forged name, an older epoch's name,
  and a burst of forged names that leaves no state behind.

Public API: a request handler on `@moq/crypto`'s broadcast producer. Wire: none.

## Required

- [`@moq/net` broadcasts expose their requests](/quest/m2/net-broadcast-requests.md) - the public handler this wraps; split out 2026-11-03 so plaintext users get it first
- [JS mirror](/quest/m2/net-wrap/js.md) - the `@moq/net` interfaces the `@moq/crypto` wrappers implement
