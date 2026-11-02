# [XS] JS tests and docs say what keeps a group cached

## Goal

No `@moq/net` test or doc assumes a track's `maxAge` controls how long the JS
cache keeps a group. That is the fixed 30 s `CACHE_WINDOW` in
`js/net/src/track.ts`. Each one either uses the window or says why it doesn't.

## Plan

Planned 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-complete run). Found by [moq-dev/moq#7219](https://github.com/moq-dev/moq/pull/7219), whose
eviction test passed with eviction disabled because it assumed `maxAge`.

- Search the JS tests and `doc/` for `maxAge` used as retention, and fix each.
- Check that each fixed test fails with eviction disabled.
