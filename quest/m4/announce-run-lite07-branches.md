# [S] AnnounceRun drops the lite-07 branches nothing reaches

## Goal

`AnnounceRun` in `rs/moq-net/src/lite/publisher.rs` answers a bidirectional
Announce stream, which only lite-06 and older open. Its lite-07 branches
(`has_synced` in the initial set and in `init`, and the encoder paths behind
them) are gone, and so is whatever in the publisher only they used.

## Plan

From [the route stream](https://github.com/moq-dev/moq/pull/8835), which moved
lite-07 to one Announce Stream answered by `lite::publisher::routed::Answer`
and refuses the bidirectional Announce stream there (`ControlType::Announce`
is unknown when `version.has_routes()`). It deleted the lite-07-only tests of
`AnnounceRun` and left the branches, to keep its diff on the wire change.

- Delete each branch of `AnnounceRun` and its helpers that only a lite-07
  version takes, then whatever becomes unused.
- `js/net`'s `Publisher.runAnnounce` has the same split: delete the lite-07
  paths the Announce Stream's `Answer` replaced.
- No behaviour changes: every existing test passes untouched.

Public API: none. Wire: none.
