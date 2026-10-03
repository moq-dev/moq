# [M] JS ranges

## Goal

`@moq/net` matches Rust: `Subscription` carries ranges and order, the lite-07
SUBSCRIBE carries them on the wire, FETCH is gone from lite-07, and
`fetchGroup` becomes a one-range subscription or is removed.

## Plan

Mirror the Rust names. The JS FETCH cancel and JS FETCH quests shape the
current `fetchGroup` surface; fold whatever of them is still open into this.

## Required

- [Lite-07 ranges](/quest/m1/subscribe-ranges/lite.md) - the wire this speaks
