# [S] Transcoders start at group boundaries

## Goal

Two moq-transcode instances serving one derived path are interchangeable at
group boundaries, so a relay moving a subscription between them never splices
two encoders' output mid-group. moq-transcode (`rs/moq-transcode`, and
`moq transcode` in moq-cli) resolves a subscription that starts at (N, M>0)
to (N+1, 0), and refuses a FETCH that starts mid-group. Output groups mirror
the source's sequence numbers, and the catalog derives only from the input and
config.

## Plan

- Decided 2026-10-04 by the maintainer: the fix is transcode-only and relay
  resume stays as it is. A transcoder cannot resume a group
  deterministically, so it never serves a partial group: a subscription
  starting at (N, M>0) skips group N for that subscriber and starts at
  (N+1, 0), and a mid-group FETCH is refused.
- moq-net answers subscriptions and cached fetches from the track cache
  before the transcoder sees them, so the refusal needs a moq-net hook. A
  `track::Info::whole_groups` flag is the hook: a local serving policy, not
  carried in TRACK_INFO, so no wire change and relays are unaffected. Where
  it lives (`Info` field, a `Producer` setter, or a per-group flag) is still
  open on the PR.
- The lite draft already lets a publisher skip a group it cannot serve from
  the requested frame and resolve to a later one, so no wire change.
- Output groups already mirror the source's sequences (`rung.rs`), pinned by
  a test of two instances fed one source.
- The maintainer wants deterministic transcodes, aligned with moq.pro's
  [wildcard transcode plan](https://github.com/moq-dev/moq.pro/blob/main/quest/m2/wildcard/transcode.md).
  Two per-process leaks into the catalog are queued as follow-up quests, not
  done here: rung names minted per process (`video/120p.2` after a resize,
  `catalog::Names`), and the probed catalog entry depending on the host's
  backend under `encode::Kind::Auto` (pin the encoder kind).

Public API: `moq_net::track::Info::whole_groups` and `with_whole_groups`.
Wire: none.

## Related

- [Wildcard](/quest/m0/wildcard/README.md) - a double claim's two workers at one path are settled by interchangeable output
