# [S] Session auth grants and replies stay consistent and bounded

## Goal

moq-net's session-side auth (`rs/moq-net/src/auth.rs`, `auth/tokens.rs`, and the AUTH stream loops in `lite/publisher.rs` and `ietf/auth.rs`) keeps four promises it currently leaves to callers:

- A token answered with AUTH_ERROR leaves the enforced scope at once. Today an AUTH_OK too large to encode is replaced by AUTH_ERROR and `state.done = true` without `Issue::finish`, so the grant `reply()` already recorded keeps admitting requests for the session.
- Replies queued for a peer that withholds stream credit are bounded: a newer `Reply::Grant` replaces a queued one instead of appending to the outbox.
- Tokens replaced by `Tokens::set` are withdrawn even when they were never answered; a backend failing transiently while the app rotates JWTs does not pile up retrying AUTH streams.
- `Handle::requests` never drops queued `Request`s under the state lock (dropping one re-enters it); take them out and drop them after the guard.

## Plan

Found by the 2026-11-07 invariant audit (lite-2, lite-5, lite-6, lite-8). Each gets a test: `allows()` is false after an oversize accept, repeated `Issued::update` on a blocked stream keeps one queued grant, two `set` calls with no answer leave one retiring generation, and swapping the acceptor with queued requests does not deadlock.
