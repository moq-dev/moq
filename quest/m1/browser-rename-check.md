# [S] A browser follows a renamed track

## Goal

A real browser keeps playing across both renames
[#5552](https://github.com/moq-dev/moq/pull/5552) introduced: a codec change,
and a `Video.Encoder` growing past its coded-size ceiling. In both cases
`@moq/publish` mints a new `track` for the same rendition, and `js/watch`
follows it without a stall. The check runs at least nightly in CI.

## Plan

#5552 landed with unit tests only and listed this check as a follow-up. Unit
tests do not show what WebCodecs and the watch path do across the switch.

Extend the browser media QA (`just test media`) rather than building a new
harness. Fail on a stall, a decode error, or a frame from the old track after
the switch. Check that it fails with the rename disabled.

Public API: none. Wire: none.

## Related

- [Rust identity enforcement](/quest/m1/catalog-identity-rs.md) - the same contract for the Rust publishers
