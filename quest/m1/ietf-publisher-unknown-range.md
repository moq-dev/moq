# [S] The Rust IETF publisher marks a lost listed group unknown

## Goal

When the Rust moq-transport publisher has listed a group in a FETCH answer or
a draft-22 fill but can no longer serve it, it sends End of Unknown Range
(`0x10C`) for that group on drafts 18 and 22, as the JS publisher does, instead
of skipping it silently.

## Plan

Found 2026-10-28 by [moq-dev/moq#7150](https://github.com/moq-dev/moq/pull/7150),
which fixed the JS publisher. In `rs/moq-net/src/ietf/publisher.rs` Rust skips
such a group in two places: the FETCH answer loop in `serve_fetch_stream`, on
`NotFound | NotFetchable | Unsupported | Old`, and `run_fill`, when
`fetch_group` fails with the same errors.

Rust can refetch from upstream, so a skip there means a failed refetch rather
than only an eviction. `NotFound` and `Old` map to `0x10C`. Decide in the PR
whether `Unsupported` and `NotFetchable` mean "unknown" too, or need their own
handling, and record the reason.

Decided 2026-10-28 (maintainer): it starts after the three quests below land,
since each edits the same publisher. The third was IETF fill timeout, which
[moq-dev/moq#7194](https://github.com/moq-dev/moq/pull/7194) dropped for
[one group](https://github.com/moq-dev/moq/pull/7204), which edited the same
paths and has landed. A FETCH or fill now lists at most one group, so this
marks that one group.

Tests: a listed group evicted before it is written, for FETCH on drafts 18
and 22 and for a draft-22 fill, each failing before the fix. Run `just test interop --all` if the framing order changes.

Public API: none. Wire: none; the marker exists in both drafts.

## Required

- [Rust FETCH parity](/quest/m1/ietf-fetch-parity.md) - edits the same FETCH answer path
- [SUBSCRIBE honors the properties opt-out](/quest/m1/subscribe-timestamps.md) - edits the same publisher
