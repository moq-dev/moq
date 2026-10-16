# [S] A browser follows a renamed track

## Goal

A real browser keeps playing when a codec identity change makes
`@moq/publish` mint a new track for the same rendition. A size-only change,
including coded-size ceiling growth, keeps the track identity and also plays
without a stall. The check runs at least nightly in CI.

## Plan

The publisher's `broadcast.ts` excludes size from decoder identity; its unit
tests cover same-track size changes. Exercise that contract through WebCodecs
and watch using the existing browser media QA (`just test media`).

Test a codec change that requires a new track and a size-only change that
must preserve it. Assert the expected identity in each case, continued
playback, and no decode errors. The codec case must fail with renaming
disabled; the size case must catch an unnecessary rename.

Public API: none. Wire: none.

## Related

- [Rust identity enforcement](/quest/m1/catalog-identity-rs.md) - the same contract for the Rust publishers
