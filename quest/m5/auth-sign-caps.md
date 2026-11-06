# [XS] moq auth sign mints capped tokens

## Goal

`moq auth sign` takes flags for the bitrate caps from
[moq-dev/moq#7535](https://github.com/moq-dev/moq/pull/7535), its publish and
subscribe rates and the object role's `rate`, so a capped token doesn't need
the library.

## Plan

Planned 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-complete run). Mirror the grant fields #7535 added, and document them in
`doc/bin/cli.md` and the relay auth guide.
