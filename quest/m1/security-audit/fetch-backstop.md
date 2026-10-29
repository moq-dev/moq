# [XS] Lite fetch backstop

## Goal

The pre-lite-05 fetch fills a lite subscriber holds (`FeedFetches` in
`rs/moq-net/src/lite/subscriber.rs`) are bounded by design, or by the
session's configured limits, not by `Limits::default().announces`.

## Plan

From the 2026-10-27 audit (NET-2). `FeedFetches::new` sets `max` from
`crate::session::Limits::default().announces` (100,000), an unrelated field's
default that ignores a session configured with tighter limits for untrusted
peers.

Prefer deleting the backstop if the held fills are already bounded by what
the cache and ledger hold (the comment says wide fills cost that); a refusal
constant whose only effect is a runtime refusal is what AGENTS.md forbids.
Otherwise thread the session's `Limits` into `FeedFetches`. Show which in the
PR with the bound's derivation.

Public API: none. Wire: none.
