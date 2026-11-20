# [L] A grant is a set of path prefixes again

## Goal

Every grant is a set of path prefixes, each covering its whole subtree, as before
[moq-dev/moq#3746](https://github.com/moq-dev/moq/pull/3746). That covers a token's publish
and subscribe lists, `moq auth serve` rules, the relay's public rules, an origin handle's
scope, and lite-07's AUTH_OK. A wildcard grant (`live/*`, `*/chat`, `cam-*.hang`) is refused
where it is made. So a route a grant admits always serves whole subtrees, and a claim's reach
is the path it is shown at ([claim reach](/quest/m2/lite07-claim-reach-routes.md)).

## Plan

Decided 2026-11-20 (maintainer, on [moq-dev/moq#9229](https://github.com/moq-dev/moq/pull/9229)):
✅ revert to prefix grants before the lite-07 cut. No use case needed a wildcard grant, and one
makes a claim's reach inexpressible through a relay (rejected: keeping wildcard grants and
carrying a claim's reach as patterns on ANNOUNCE_START and ANNOUNCE_UPDATE).

- Back out the grant half of #3746 and the pattern-valued scopes of
  [moq-dev/moq#3672](https://github.com/moq-dev/moq/pull/3672):
  - `Producer::scope`, `Consumer::scope`, and their `allowed` take and return prefixes, in
    Rust (`rs/moq-net/src/model/origin.rs`, `OriginScope`) and JS (`js/net`).
  - `Permit` (`rs/moq-net/src/auth.rs`) holds prefixes.
  - moq-auth and @moq/auth tokens, `moq auth serve` rules, the relay's `public*` config,
    moq-ffi, and the bindings carry prefixes.
  - Anything that only existed to intersect wildcard grants goes with it, such as
    `Consumer::reaches_any` where a prefix test now does.
  - Discovery filters, which choose the claims a reader looks at (announcement captures), keep
    their patterns: reach does not concern them.
- Wire: lite-07's AUTH_OK carries Publish and Subscribe Prefixes, and the region rule's regions
  (`drafts/draft-lcurley-moq-lite.md`, Announce Request) are the scope's prefixes. The Path
  Pattern section goes, with any other use the draft keeps. lite-07 is unpublished, so nothing
  stays compatible with the pattern form.
- Syntax, recommended: a member `foo` covers `foo` and its subtree, and the empty member covers
  everything. A trailing `/**` (and a bare `**`) parses as the same prefix, since it names exactly
  that set, so deployed `anon/**` tokens and configs keep their meaning. Every other wildcard is
  refused where it is parsed. The migration note warns that a bare member minted since #3746
  widens from one broadcast to its subtree.
- Align the quests that assume wildcard members: `quest/m4/regrant-route-recheck.md` (a member's
  literal prefix before its first wildcard) and `quest/m5/peer-grant-bound.md` (the Orthogonal
  Vectors bound, which prefixes remove).
- Docs: `doc/bin/relay/auth.md` (Path matching and the rule tables), and each library page that
  shows a wildcard grant.

Public API: breaking (grant types and token contents, in Rust, JS, and the bindings). Wire:
lite-07 AUTH_OK.
