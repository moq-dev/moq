# [M] Encrypted JS broadcasts serve undeclared track names

## Goal

`@moq/net` exposes a public broadcast request handler, and `@moq/e2ee`'s
broadcast producer answers requests for track names it never created
through it, under semantic names, as `moq_e2ee::broadcast::Requests` does in
Rust. A JS
application then serves on-demand tracks from a plaintext or encrypted
broadcast the same way.

## Plan

Decided with the maintainer on 2026-10-24:

- One quest, both halves. Today the JS broadcast's request pull
  (`requested()` in `js/net/src/broadcast.ts`) is reachable only by wire
  layers, and only the origin has a public `requests()`. Expose the
  broadcast's handler under Rust's name and shape (`requests()` returning
  `Requests`), and keep the internal wire path on it.
- `@moq/e2ee` mirrors the Rust handler exactly: it opens each request's
  physical name, claims it, and yields a request whose `name` is semantic.
  A name that does not open is answered as missing and keeps no state. A
  re-requested or re-created name resumes above its high-water marks, as in
  Rust (decided 2026-10-25, maintainer: resume always, replacing the
  2026-10-17 refusal, so every identity stays unique and a handler can serve
  a dropped name again).
- The handler offers the lister, taking semantic names and listing their
  physical names, as Rust's `moq_e2ee::broadcast::Lister` does. `@moq/net`'s broadcast lister is
  internal today (tests reach it through `wireOf`), so export it with the
  handler.
- Port Rust's `requests_refuse_names_the_generation_did_not_issue` and
  `requests_serve_a_dropped_name_above_its_marks` tests: a relay-forged name, an older epoch's name,
  and a burst of forged names that leaves no state behind.

Public API: a public broadcast `requests()` in `@moq/net`, and a request
handler on `@moq/e2ee`'s broadcast producer. Wire: none.

## Required

- [JS mirror](/quest/m2/net-wrap/js.md) - the `@moq/net` interfaces the `@moq/e2ee` wrappers implement
